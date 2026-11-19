// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Stairs and railings (schema 14). A stair is defined by bottom/top levels, shape (straight, L, U),
//! width, maximum riser and tread (or the 2R + T rule); riser count, landings, stringers, exact
//! solids and quantities come from `ddc_geom::stair`. A railing follows a stair edge or a sketch;
//! `stair_qty`/`railing_qty` are derived.

use ddc_geom::section::{Prism, Zfn};
use ddc_geom::stair::{
    Railing, RailingLayout, RailingQty, Shape, Stair, StairDef, StairPlan, StairQty,
};
use ddc_geom::{v2, v3};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

#[derive(Clone, Debug, PartialEq)]
pub struct StairSpec {
    pub level: i64,
    pub top_level: i64,
    pub base_offset: f64,
    pub top_offset: f64,
    pub shape: Shape,
    /// Midpoint of the bottom line of the first riser.
    pub origin: (f64, f64),
    /// Direction of ascent, rad.
    pub angle: f64,
    pub width: f64,
    pub max_riser: f64,
    pub tread: Option<f64>,
    pub landing: f64,
    pub thickness: f64,
    pub stringer: Option<(f64, f64)>,
    pub material: String,
}

impl StairSpec {
    pub fn straight(level: i64, top_level: i64, origin: (f64, f64), angle: f64) -> Self {
        StairSpec {
            level,
            top_level,
            base_offset: 0.0,
            top_offset: 0.0,
            shape: Shape::Straight,
            origin,
            angle,
            width: 1.2,
            max_riser: 0.18,
            tread: None,
            landing: 1.2,
            thickness: 0.15,
            stringer: None,
            material: "Concrete".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RailingHost {
    /// Stair edge: 0 is left, 1 is right (in the direction of ascent).
    Stair { stair: i64, side: u8 },
    /// Sketch: points (x, y, z relative to the level).
    Path(Vec<(f64, f64, f64)>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RailingSpec {
    pub level: i64,
    pub host: RailingHost,
    pub height: f64,
    pub post_spacing: f64,
    pub baluster_spacing: f64,
    pub post: f64,
    pub baluster: f64,
    pub rail: (f64, f64),
    pub material: String,
}

impl RailingSpec {
    pub fn new(level: i64, host: RailingHost) -> Self {
        RailingSpec {
            level,
            host,
            height: 0.9,
            post_spacing: 1.0,
            baluster_spacing: 0.12,
            post: 0.05,
            baluster: 0.02,
            rail: (0.05, 0.04),
            material: "Steel".into(),
        }
    }
}

fn elev(db: &Connection, l: i64) -> Result<f64, Error> {
    db.query_row("SELECT elevation FROM level WHERE id=?1", [l], |r| r.get(0))
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no level {l}")))
}

fn def(db: &Connection, s: &StairSpec) -> Result<StairDef, Error> {
    let z0 = elev(db, s.level)? + s.base_offset;
    let z1 = elev(db, s.top_level)? + s.top_offset;
    Ok(StairDef {
        origin: v2(s.origin.0, s.origin.1),
        angle: s.angle,
        base_z: z0,
        height: z1 - z0,
        width: s.width,
        max_riser: s.max_riser,
        tread: s.tread,
        landing: s.landing,
        thickness: s.thickness,
        stringer: s.stringer,
        shape: s.shape,
    })
}

fn layout(db: &Connection, s: &StairSpec) -> Result<Stair, Error> {
    def(db, s)?
        .layout()
        .map_err(|e| Error::Invalid(format!("stair: {e:?}")))
}

pub(crate) fn load(db: &Connection, id: i64) -> Result<StairSpec, Error> {
    db.query_row(
        "SELECT level,top_level,base_offset,top_offset,kind,turn,x,y,angle,width,max_riser,tread,first,\
         gap,landing,thickness,stringer_w,stringer_h,material,spiral_r,spiral_core,spiral_turn FROM stair WHERE id=?1",
        [id],
        |r| {
            let kind: String = r.get(4)?;
            let left = r.get::<_, i64>(5)? != 0;
            let first: Option<i64> = r.get(12)?;
            let first = first.map(|v| v as usize);
            let gap: f64 = r.get(13)?;
            let shape = match kind.as_str() {
                "l" => Shape::L { left, first },
                "u" => Shape::U { left, first, gap },
                "spiral" => Shape::Spiral {
                    radius: r.get::<_, Option<f64>>(19)?.unwrap_or(0.0),
                    core: r.get::<_, Option<f64>>(20)?.unwrap_or(0.0),
                    turn: r.get::<_, Option<f64>>(21)?.unwrap_or(0.0),
                },
                _ => Shape::Straight,
            };
            let sw: Option<f64> = r.get(16)?;
            let sh: Option<f64> = r.get(17)?;
            Ok(StairSpec {
                level: r.get(0)?,
                top_level: r.get(1)?,
                base_offset: r.get(2)?,
                top_offset: r.get(3)?,
                shape,
                origin: (r.get(6)?, r.get(7)?),
                angle: r.get(8)?,
                width: r.get(9)?,
                max_riser: r.get(10)?,
                tread: r.get(11)?,
                landing: r.get(14)?,
                thickness: r.get(15)?,
                stringer: sw.zip(sh),
                material: r.get(18)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("no such stair {id}")))
}

fn write(db: &Connection, s: &StairSpec, id: Option<i64>) -> Result<i64, Error> {
    let (kind, left, first, gap, spiral) = match s.shape {
        Shape::Straight => ("straight", false, None, 0.0, None),
        Shape::L { left, first } => ("l", left, first, 0.0, None),
        Shape::U { left, first, gap } => ("u", left, first, gap, None),
        Shape::Spiral { radius, core, turn } => ("spiral", false, None, 0.0, Some((radius, core, turn))),
    };
    let (spr, spc, spt) = spiral.map_or((None, None, None), |(a, b, c)| (Some(a), Some(b), Some(c)));
    db.execute(
        "INSERT OR REPLACE INTO stair(id,level,top_level,base_offset,top_offset,kind,turn,x,y,angle,width,\
         max_riser,tread,first,gap,landing,thickness,stringer_w,stringer_h,material,spiral_r,spiral_core,spiral_turn) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)",
        params![
            id,
            s.level,
            s.top_level,
            s.base_offset,
            s.top_offset,
            kind,
            left as i64,
            s.origin.0,
            s.origin.1,
            s.angle,
            s.width,
            s.max_riser,
            s.tread,
            first.map(|v| v as i64),
            gap,
            s.landing,
            s.thickness,
            s.stringer.map(|v| v.0),
            s.stringer.map(|v| v.1),
            s.material,
            spr,
            spc,
            spt
        ],
    )?;
    Ok(db.last_insert_rowid())
}

/// Turns a stair-hosted railing into a sketch railing at its current absolute position.
pub(crate) fn detach_railing(db: &Connection, id: i64) -> Result<(), Error> {
    let def = railing_def(db, id)?;
    let level: i64 = db.query_row("SELECT level FROM railing WHERE id=?1", [id], |r| r.get(0))?;
    let z0 = elev(db, level)?;
    db.execute("DELETE FROM railing_vertex WHERE railing=?1", [id])?;
    for (i, p) in def.path.iter().enumerate() {
        db.execute(
            "INSERT INTO railing_vertex VALUES (?1,?2,?3,?4,?5)",
            params![id, i as i64, p.x, p.y, p.z - z0],
        )?;
    }
    db.execute("UPDATE railing SET stair=NULL, side=NULL WHERE id=?1", [id])?;
    Ok(())
}

/// Railing path in world elevations.
fn railing_def(db: &Connection, id: i64) -> Result<Railing, Error> {
    type Row = (i64, Option<i64>, Option<i64>, [f64; 7]);
    let h: Row = db
        .query_row(
            "SELECT level, stair, side, height, post_spacing, baluster_spacing, post, baluster, rail_w, rail_h \
             FROM railing WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    [r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?],
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no such railing {id}")))?;
    let path = match h.1 {
        Some(st) => {
            let s = layout(db, &load(db, st)?)?;
            let side = h.2.unwrap_or(0).clamp(0, 1) as usize;
            s.edge_paths()[side].clone()
        }
        None => {
            let z = elev(db, h.0)?;
            let mut st =
                db.prepare("SELECT x, y, z FROM railing_vertex WHERE railing=?1 ORDER BY idx")?;
            let v: Vec<(f64, f64, f64)> = st
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?;
            v.into_iter().map(|p| v3(p.0, p.1, p.2 + z)).collect()
        }
    };
    let v = h.3;
    Ok(Railing {
        path,
        height: v[0],
        post_spacing: v[1],
        baluster_spacing: v[2],
        post: v[3],
        baluster: v[4],
        rail: (v[5], v[6]),
    })
}

fn spec_railing(db: &Connection, s: &RailingSpec) -> Result<Railing, Error> {
    let path = match &s.host {
        RailingHost::Stair { stair, side } => {
            let st = layout(db, &load(db, *stair)?)?;
            st.edge_paths()[(*side).min(1) as usize].clone()
        }
        RailingHost::Path(p) => {
            let z = elev(db, s.level)?;
            p.iter().map(|q| v3(q.0, q.1, q.2 + z)).collect()
        }
    };
    let r = Railing {
        path,
        height: s.height,
        post_spacing: s.post_spacing,
        baluster_spacing: s.baluster_spacing,
        post: s.post,
        baluster: s.baluster,
        rail: s.rail,
    };
    r.check()
        .map_err(|e| Error::Invalid(format!("railing: {e}")))?;
    Ok(r)
}

fn write_railing(db: &Connection, s: &RailingSpec) -> Result<i64, Error> {
    write_railing_as(db, s, None)
}

fn write_railing_as(db: &Connection, s: &RailingSpec, row: Option<i64>) -> Result<i64, Error> {
    let (stair, side) = match &s.host {
        RailingHost::Stair { stair, side } => (Some(*stair), Some(*side as i64)),
        RailingHost::Path(_) => (None, None),
    };
    db.execute(
        "INSERT INTO railing(id,level,stair,side,height,post_spacing,baluster_spacing,post,baluster,rail_w,rail_h,material) \
         VALUES (?12,?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            s.level,
            stair,
            side,
            s.height,
            s.post_spacing,
            s.baluster_spacing,
            s.post,
            s.baluster,
            s.rail.0,
            s.rail.1,
            s.material,
            row
        ],
    )?;
    let id = db.last_insert_rowid();
    if let RailingHost::Path(p) = &s.host {
        for (i, q) in p.iter().enumerate() {
            db.execute(
                "INSERT INTO railing_vertex VALUES (?1,?2,?3,?4,?5)",
                params![id, i as i64, q.0, q.1, q.2],
            )?;
        }
    }
    Ok(id)
}

fn ids_where(db: &Connection, sql: &str, level: i64) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare(sql)?;
    let r = st.query_map([level], |r| r.get(0))?;
    Ok(r.collect::<Result<_, _>>()?)
}

fn ids(db: &Connection, sql: &str) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare(sql)?;
    let r = st.query_map([], |r| r.get(0))?;
    Ok(r.collect::<Result<_, _>>()?)
}

pub(crate) fn regenerate(db: &Connection) -> Result<(), Error> {
    db.execute("DELETE FROM stair_qty", [])?;
    db.execute("DELETE FROM railing_qty", [])?;
    for id in ids(db, "SELECT id FROM stair ORDER BY id")? {
        let Ok(s) = layout(db, &load(db, id)?) else {
            continue;
        };
        let q = s.quantities();
        db.execute(
            "INSERT INTO stair_qty VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                id,
                q.risers as i64,
                q.treads as i64,
                q.riser,
                q.tread,
                q.run_length,
                q.volume,
                q.landing_volume,
                q.stringer_volume
            ],
        )?;
    }
    for id in ids(db, "SELECT id FROM railing ORDER BY id")? {
        let Ok(r) = railing_def(db, id) else { continue };
        if r.check().is_err() {
            continue;
        }
        let q = r.quantities();
        db.execute(
            "INSERT INTO railing_qty VALUES (?1,?2,?3,?4,?5)",
            params![
                id,
                q.length,
                q.posts as i64,
                q.balusters as i64,
                q.post_volume + q.baluster_volume + q.rail_volume
            ],
        )?;
    }
    Ok(())
}

/// Stair prisms for sections: flights (top is the steps, bottom is a sloped slab) and landings.
pub(crate) fn prisms(db: &Connection) -> Result<Vec<(i64, String, Prism)>, Error> {
    let mut out = vec![];
    for id in ids(db, "SELECT id FROM stair ORDER BY id")? {
        let spec = load(db, id)?;
        let Ok(s) = layout(db, &spec) else { continue };
        for f in &s.flights {
            let zf = |prof: Vec<(f64, f64)>| Zfn {
                o: f.start,
                z: f.z0,
                grad: v2(0.0, 0.0),
                dir: f.dir,
                prof,
            };
            out.push((
                id,
                spec.material.clone(),
                Prism {
                    rings: vec![f.ring(s.tread)],
                    bottom: zf(s.flight_bottom(f)),
                    top: zf(s.flight_top(f)),
                    voids: vec![],
                },
            ));
        }
        for l in &s.landings {
            out.push((
                id,
                spec.material.clone(),
                Prism {
                    rings: vec![l.ring.clone()],
                    bottom: Zfn::flat(l.top - s.thickness),
                    top: Zfn::flat(l.top),
                    voids: vec![],
                },
            ));
        }
    }
    Ok(out)
}

/// Checked stair insert for importers (`id` = Some: replace that row in place).
pub fn put_stair(db: &Connection, s: &StairSpec, id: Option<i64>) -> Result<i64, Error> {
    layout(db, s)?;
    write(db, s, id)
}

/// Checked railing insert for importers; with `id` the old row and its vertices are replaced.
pub fn put_railing(db: &Connection, s: &RailingSpec, id: Option<i64>) -> Result<i64, Error> {
    spec_railing(db, s)?;
    if let Some(old) = id {
        db.execute("DELETE FROM railing_vertex WHERE railing=?1", [old])?;
        db.execute("DELETE FROM railing WHERE id=?1", [old])?;
    }
    write_railing_as(db, s, id)
}

impl Model {
    /// Stairs on `level`, by id.
    pub fn stairs(&self, level: i64) -> Result<Vec<i64>, Error> {
        ids_where(&self.db, "SELECT id FROM stair WHERE level=?1 ORDER BY id", level)
    }

    /// Railings on `level`, by id.
    pub fn railings(&self, level: i64) -> Result<Vec<i64>, Error> {
        ids_where(&self.db, "SELECT id FROM railing WHERE level=?1 ORDER BY id", level)
    }

    pub fn add_stair(&mut self, s: &StairSpec) -> Result<i64, Error> {
        layout(&self.db, s)?;
        self.op("stair_add", |db| {
            let id = write(db, s, None)?;
            Ok((id, id, Inv::default().delete("stair", "id", id)))
        })
    }

    pub fn update_stair(&mut self, id: i64, s: &StairSpec) -> Result<(), Error> {
        layout(&self.db, s)?;
        self.op("stair_edit", |db| {
            load(db, id)?;
            let inv = Inv::default().restore(db, "stair", "id", id)?;
            write(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_stair(&mut self, id: i64) -> Result<(), Error> {
        self.delete_ref("stair", "stair_delete", id, &[])
    }

    pub fn stair(&self, id: i64) -> Result<StairSpec, Error> {
        load(&self.db, id)
    }

    /// Layout: risers, tread, flights, landings.
    pub fn stair_layout(&self, id: i64) -> Result<Stair, Error> {
        layout(&self.db, &load(&self.db, id)?)
    }

    pub fn stair_quantities(&self, id: i64) -> Result<StairQty, Error> {
        Ok(self.stair_layout(id)?.quantities())
    }

    pub fn stair_meshes(&self, id: i64) -> Result<Vec<ddc_geom::mesh::Mesh>, Error> {
        Ok(self.stair_layout(id)?.meshes())
    }

    /// Stair plan for cut height `cut_height` above level `level`.
    pub fn stair_plan(&self, id: i64, level: i64, cut_height: f64) -> Result<StairPlan, Error> {
        let z = elev(&self.db, level)? + cut_height;
        Ok(self.stair_layout(id)?.plan(z))
    }

    pub fn add_railing(&mut self, s: &RailingSpec) -> Result<i64, Error> {
        spec_railing(&self.db, s)?;
        self.op("railing_add", |db| {
            let id = write_railing(db, s)?;
            let inv = Inv::default()
                .delete("railing_vertex", "railing", id)
                .delete("railing", "id", id);
            Ok((id, id, inv))
        })
    }

    /// Railings along both stair edges in one operation.
    pub fn add_stair_railings(&mut self, stair: i64, tpl: &RailingSpec) -> Result<[i64; 2], Error> {
        let level = load(&self.db, stair)?.level;
        let specs: Vec<RailingSpec> = (0..2u8)
            .map(|side| RailingSpec {
                level,
                host: RailingHost::Stair { stair, side },
                ..tpl.clone()
            })
            .collect();
        for s in &specs {
            spec_railing(&self.db, s)?;
        }
        self.op("railing_add", |db| {
            let a = write_railing(db, &specs[0])?;
            let b = write_railing(db, &specs[1])?;
            let inv = Inv::default()
                .delete("railing", "id", a)
                .delete("railing", "id", b);
            Ok(([a, b], a, inv))
        })
    }

    pub fn delete_railing(&mut self, id: i64) -> Result<(), Error> {
        self.delete_ref(
            "railing",
            "railing_delete",
            id,
            &[("railing_vertex", "railing")],
        )
    }

    /// The whole path of a sketch railing (x, y, z relative to the level): vertex editing via handles.
    pub fn set_railing_path(&mut self, id: i64, pts: &[(f64, f64, f64)]) -> Result<(), Error> {
        if pts.len() < 2 {
            return Err(Error::Invalid("railing path needs 2+ points".into()));
        }
        self.op("railing_path", |db| {
            let n: i64 = db.query_row(
                "SELECT count(*) FROM railing WHERE id=?1 AND stair IS NULL",
                [id],
                |r| r.get(0),
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no sketch railing {id}")));
            }
            let inv = Inv::default()
                .delete("railing_vertex", "railing", id)
                .restore(db, "railing_vertex", "railing", id)?;
            db.execute("DELETE FROM railing_vertex WHERE railing=?1", [id])?;
            for (k, p) in pts.iter().enumerate() {
                db.execute(
                    "INSERT INTO railing_vertex(railing, idx, x, y, z) VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![id, k as i64, p.0, p.1, p.2],
                )?;
            }
            Ok(((), id, inv))
        })
    }

    /// Path points of a sketch railing (empty means the railing follows the stair).
    /// Stored railing parameters; the path or host is reported, not derived.
    pub fn railing(&self, id: i64) -> Result<RailingSpec, Error> {
        type Row = (i64, Option<i64>, Option<i64>, [f64; 7], String);
        let h: Row = self
            .db
            .query_row(
                "SELECT level, stair, side, height, post_spacing, baluster_spacing, post, baluster, \
                 rail_w, rail_h, material FROM railing WHERE id=?1",
                [id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        [r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?],
                        r.get(10)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no such railing {id}")))?;
        let host = match h.1 {
            Some(stair) => RailingHost::Stair {
                stair,
                side: h.2.unwrap_or(0).clamp(0, 1) as u8,
            },
            None => RailingHost::Path(self.railing_path(id)?),
        };
        let v = h.3;
        Ok(RailingSpec {
            level: h.0,
            host,
            height: v[0],
            post_spacing: v[1],
            baluster_spacing: v[2],
            post: v[3],
            baluster: v[4],
            rail: (v[5], v[6]),
            material: h.4,
        })
    }

    /// Height, spacing and section sizes; level and host keep their own edits.
    pub fn update_railing(&mut self, id: i64, s: &RailingSpec) -> Result<(), Error> {
        spec_railing(&self.db, s)?;
        self.op("railing_edit", |db| {
            let inv = Inv::default().restore(db, "railing", "id", id)?;
            let n = db.execute(
                "UPDATE railing SET height=?2, post_spacing=?3, baluster_spacing=?4, post=?5, \
                 baluster=?6, rail_w=?7, rail_h=?8, material=?9 WHERE id=?1",
                params![
                    id,
                    s.height,
                    s.post_spacing,
                    s.baluster_spacing,
                    s.post,
                    s.baluster,
                    s.rail.0,
                    s.rail.1,
                    s.material
                ],
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no such railing {id}")));
            }
            Ok(((), id, inv))
        })
    }

    pub fn railing_path(&self, id: i64) -> Result<Vec<(f64, f64, f64)>, Error> {
        let mut st = self
            .db
            .prepare("SELECT x, y, z FROM railing_vertex WHERE railing=?1 ORDER BY idx")?;
        let r = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    pub fn railing_layout(&self, id: i64) -> Result<RailingLayout, Error> {
        Ok(railing_def(&self.db, id)?.layout())
    }

    pub fn railing_quantities(&self, id: i64) -> Result<RailingQty, Error> {
        Ok(railing_def(&self.db, id)?.quantities())
    }

    pub fn railing_meshes(&self, id: i64) -> Result<Vec<ddc_geom::mesh::Mesh>, Error> {
        Ok(railing_def(&self.db, id)?.meshes())
    }

    /// The railing's path and sizes, for a viewer that draws it in more detail.
    pub fn railing_geom(&self, id: i64) -> Result<Railing, Error> {
        railing_def(&self.db, id)
    }
}

/// Spiral columns `stair.spiral_r/spiral_core/spiral_turn` on files made before them.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    for col in ["spiral_r", "spiral_core", "spiral_turn"] {
        let has: i64 = db.query_row(
            "SELECT count(*) FROM pragma_table_info('stair') WHERE name=?1",
            [col],
            |r| r.get(0),
        )?;
        if has == 0 {
            db.execute_batch(&format!("ALTER TABLE stair ADD COLUMN {col} REAL;"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SectionSpec, ViewKind, ViewPrim};

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let a = m.add_level("L1", 0.0).unwrap();
        let b = m.add_level("L2", 3.0).unwrap();
        (m, a, b)
    }

    #[test]
    fn stair_3000_has_17_risers_of_176_47() {
        let (mut m, a, b) = model();
        let id = m
            .add_stair(&StairSpec::straight(a, b, (0.0, 0.0), 0.0))
            .unwrap();
        let q = m.stair_quantities(id).unwrap();
        assert_eq!((q.risers, q.treads), (17, 16));
        assert_eq!(format!("{:.2}", q.riser * 1000.0), "176.47");
        assert!((q.tread - (0.63 - 6.0 / 17.0)).abs() < 1e-15);
        assert!((0.60..=0.65).contains(&q.comfort));
        let row: (i64, f64) = m
            .sql()
            .query_row(
                "SELECT risers, volume FROM stair_qty WHERE stair=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row.0, 17);
        let mv: f64 = m.stair_meshes(id).unwrap().iter().map(|m| m.volume()).sum();
        assert!((mv - row.1).abs() < 1e-9);
        // 2R+T out of range: rejected without writing
        let mut bad = StairSpec::straight(a, b, (0.0, 0.0), 0.0);
        bad.tread = Some(0.35);
        assert!(m.add_stair(&bad).is_err());
        // a section along the flight sees the steps
        let v = m
            .section_view(
                &SectionSpec {
                    kind: ViewKind::Section,
                    name: "S".into(),
                    a: (-1.0, 0.0),
                    b: (6.0, 0.0),
                    depth: 5.0,
                    far_clip: true,
                },
                &[],
            )
            .unwrap();
        assert!(v
            .prims
            .iter()
            .any(|p| matches!(p, ViewPrim::SectionCut { table: "stair", .. })));
        let pl = m.stair_plan(id, a, 1.2).unwrap();
        assert_eq!((pl.below.len(), pl.above.len()), (6, 11));
        m.assert_integrity();
    }

    #[test]
    fn spiral_stair_round_trips_and_builds_winders() {
        let (mut m, a, b) = model();
        let mut s = StairSpec::straight(a, b, (0.0, 0.0), 0.0);
        s.shape = Shape::Spiral { radius: 1.2, core: 0.3, turn: 4.5 };
        s.max_riser = 0.2;
        let id = m.add_stair(&s).unwrap();
        assert_eq!(m.stair(id).unwrap().shape, s.shape);
        let q = m.stair_quantities(id).unwrap();
        assert_eq!(q.risers, 15);
        assert!(q.walk_area > 0.0 && q.volume > 0.0);
        m.assert_integrity();
    }

    #[test]
    fn railings_along_l_stair_and_sketch() {
        let (mut m, a, b) = model();
        let mut s = StairSpec::straight(a, b, (0.0, 0.0), 0.0);
        s.shape = Shape::L {
            left: true,
            first: None,
        };
        s.stringer = Some((0.05, 0.3));
        let st = m.add_stair(&s).unwrap();
        let q = m.stair_quantities(st).unwrap();
        assert!(q.stringer_volume > 0.0 && q.landing_volume > 0.0);
        let [l, r] = m
            .add_stair_railings(st, &RailingSpec::new(a, RailingHost::Path(vec![])))
            .unwrap();
        assert!(m.railing_quantities(r).unwrap().length > m.railing_quantities(l).unwrap().length);
        assert!(matches!(m.delete_stair(st), Err(Error::Referenced { .. })));
        m.undo().unwrap();
        assert_eq!(
            m.sql()
                .query_row("SELECT count(*) FROM railing", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        let rid = m
            .add_railing(&RailingSpec::new(
                a,
                RailingHost::Path(vec![(0.0, 5.0, 0.0), (4.2, 5.0, 0.0)]),
            ))
            .unwrap();
        let q = m.railing_quantities(rid).unwrap();
        assert_eq!(q.posts, 6);
        assert_eq!(q.balusters, 30);
        let n: i64 = m
            .sql()
            .query_row(
                "SELECT posts FROM railing_qty WHERE railing=?1",
                [rid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 6);
        m.delete_stair(st).unwrap();
        m.assert_integrity();
    }
}
