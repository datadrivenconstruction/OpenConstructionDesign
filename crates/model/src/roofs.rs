//! Roofs (schema 13): from an outline with a slope per edge (hip, gable, shed, mansard) or by
//! extruding a profile; layers are a slab type (thickness along the slope normal).
//! `roof_qty` is derived: the true area and volume of each layer (area × thickness).
//! A wall top is attached to the roof underside (`wall.top_roof`): the top profile is recomputed
//! in the same operation as the roof edit or the attachment.

use ddc_geom::mesh::Mesh;
use ddc_geom::roof::{Extrusion, Footprint, Roof, RoofQty};
use ddc_geom::shape_edit::cut_roof;
use ddc_geom::v2;
use rusqlite::{params, Connection};

use crate::ops::Inv;
use crate::{Error, Issue, Model};

#[derive(Clone, Debug, PartialEq)]
pub enum RoofKind {
    /// Outline along the bearing lines; slope of edge i (vertex i -> i+1) in degrees, None is a gable;
    /// mansard: (break height above the bearing, upper slope in degrees; None is a flat deck).
    Footprint {
        ring: Vec<(f64, f64)>,
        slopes: Vec<Option<f64>>,
        mansard: Option<(f64, Option<f64>)>,
    },
    /// Profile (u, z) over segment a→b, extruded to the left by depth.
    Extrusion {
        a: (f64, f64),
        b: (f64, f64),
        profile: Vec<(f64, f64)>,
        depth: f64,
    },
    /// A flat slope by a slope arrow over the outline: from `tail` to `head` rising by `rise`;
    /// edges carry no slope, and ends above the eave are gables.
    Arrow {
        ring: Vec<(f64, f64)>,
        tail: (f64, f64),
        head: (f64, f64),
        rise: f64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoofSpec {
    pub level: i64,
    /// Slab type: roof layers from top to bottom.
    pub slab_type: i64,
    /// Elevation of the roof planes above the bearing line (for an extrusion, above the profile zero).
    pub offset: f64,
    pub overhang: f64,
    /// Fascia board height (quantity only).
    pub fascia: f64,
    pub kind: RoofKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoofQuantities {
    pub geom: RoofQty,
    /// (material, thickness, area, volume) per layer from top to bottom.
    pub layers: Vec<(String, f64, f64, f64)>,
    pub thickness: f64,
    pub fascia_area: f64,
}

fn invalid<T>(m: String) -> Result<T, Error> {
    Err(Error::Invalid(m))
}

/// Total thickness of the roof layers.
pub(crate) fn layer_sum(db: &Connection, stype: i64) -> Result<f64, Error> {
    Ok(layers(db, stype)?.iter().map(|l| l.1).sum())
}

fn layers(db: &Connection, stype: i64) -> Result<Vec<(String, f64)>, Error> {
    let mut st =
        db.prepare("SELECT material, thickness FROM slab_type_layer WHERE stype=?1 ORDER BY pos")?;
    let r = st.query_map([stype], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(r.collect::<Result<_, _>>()?)
}

pub(crate) fn load(db: &Connection, id: i64) -> Result<RoofSpec, Error> {
    type Head = (
        i64,
        i64,
        String,
        [f64; 3],
        Option<f64>,
        Option<f64>,
        [Option<f64>; 5],
    );
    let h: Head = db
        .query_row(
            "SELECT level, stype, kind, offset, overhang, fascia, mansard_h, mansard_deg, ax, ay, bx, by, depth \
             FROM roof WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    [r.get(3)?, r.get(4)?, r.get(5)?],
                    r.get(6)?,
                    r.get(7)?,
                    [r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?, r.get(12)?],
                ))
            },
        )
        .map_err(|_| Error::Invalid(format!("no such roof {id}")))?;
    let mut st = db.prepare("SELECT x, y, slope FROM roof_vertex WHERE roof=?1 ORDER BY idx")?;
    let vs: Vec<(f64, f64, Option<f64>)> = st
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let kind = if h.2 == "arrow" {
        let e = h.6.map(|v| v.unwrap_or(0.0));
        RoofKind::Arrow {
            ring: vs.iter().map(|v| (v.0, v.1)).collect(),
            tail: (e[0], e[1]),
            head: (e[2], e[3]),
            rise: e[4],
        }
    } else if h.2 == "extrusion" {
        let e = h.6.map(|v| v.unwrap_or(0.0));
        RoofKind::Extrusion {
            a: (e[0], e[1]),
            b: (e[2], e[3]),
            profile: vs.iter().map(|v| (v.0, v.1)).collect(),
            depth: e[4],
        }
    } else {
        RoofKind::Footprint {
            ring: vs.iter().map(|v| (v.0, v.1)).collect(),
            slopes: vs.iter().map(|v| v.2).collect(),
            mansard: h.4.map(|hb| (hb, h.5)),
        }
    };
    Ok(RoofSpec {
        level: h.0,
        slab_type: h.1,
        offset: h.3[0],
        overhang: h.3[1],
        fascia: h.3[2],
        kind,
    })
}

pub(crate) fn geometry(db: &Connection, s: &RoofSpec) -> Result<Roof, Error> {
    let elev: f64 = db
        .query_row("SELECT elevation FROM level WHERE id=?1", [s.level], |r| {
            r.get(0)
        })
        .map_err(|_| Error::Invalid(format!("roof: no level {}", s.level)))?;
    let g = |e| Error::Geom(format!("roof: {e:?}"));
    match &s.kind {
        RoofKind::Footprint {
            ring,
            slopes,
            mansard,
        } => Footprint {
            ring: ring.iter().map(|p| v2(p.0, p.1)).collect(),
            slopes: slopes.clone(),
            base_z: elev + s.offset,
            overhang: s.overhang,
            mansard: *mansard,
        }
        .build()
        .map_err(g),
        RoofKind::Extrusion {
            a,
            b,
            profile,
            depth,
        } => Extrusion {
            a: v2(a.0, a.1),
            b: v2(b.0, b.1),
            profile: profile.clone(),
            depth: *depth,
        }
        .build(elev + s.offset)
        .map_err(g),
        RoofKind::Arrow {
            ring,
            tail,
            head,
            rise,
        } => {
            let pl = ddc_geom::slope::Plane::arrow(v2(tail.0, tail.1), v2(head.0, head.1), *rise)
                .ok_or_else(|| Error::Invalid("roof: degenerate slope arrow".into()))?;
            let r: Vec<_> = ring.iter().map(|p| v2(p.0, p.1)).collect();
            if r.len() < 3 || ddc_geom::poly::area(&r).abs() < 1e-9 {
                return Err(Error::Invalid("roof: arrow ring".into()));
            }
            Ok(ddc_geom::shape_edit::arrow_roof(&r, &pl, elev + s.offset))
        }
    }
}

pub(crate) fn check(db: &Connection, s: &RoofSpec) -> Result<Roof, Error> {
    let ls = layers(db, s.slab_type)?;
    if ls.is_empty() {
        return invalid(format!("roof: slab type {} has no layers", s.slab_type));
    }
    if ![s.offset, s.overhang, s.fascia]
        .iter()
        .all(|v| v.is_finite())
        || s.overhang < 0.0
        || s.fascia < 0.0
    {
        return invalid("roof: offset/overhang/fascia".into());
    }
    geometry(db, s)
}

/// Shaft `pieces` spanning (z0, z1) reach the roof body above them: from the underside (thickness
/// `t` off the top plane) to the top at the shaft's corners. A shaft ending below a pitched roof
/// in the attic leaves it whole.
pub(crate) fn shaft_hits(g: &Roof, pieces: &[Vec<ddc_geom::V2>], t: f64, z0: f64, z1: f64) -> bool {
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for p in pieces.iter().flatten() {
        if let (Some(top), Some(under)) = (g.top_at(*p), g.underside_at(*p, t)) {
            lo = lo.min(under);
            hi = hi.max(top);
        }
    }
    lo <= hi && z1 > lo + 1e-9 && z0 < hi - 1e-9
}

/// A roof with cutouts: sketch openings and shafts that reach it in height.
pub(crate) fn cut_geometry(db: &Connection, id: i64, s: &RoofSpec) -> Result<Roof, Error> {
    let g = open_geometry(db, id, s)?;
    let t: f64 = layers(db, s.slab_type)?.iter().map(|l| l.1).sum();
    let mut out = g.clone();
    for (_, pieces, z0, z1) in crate::shafts::shaft_pieces(db)? {
        if shaft_hits(&g, &pieces, t, z0, z1) {
            for p in &pieces {
                out = cut_roof(&out, p, false);
            }
        }
    }
    Ok(out)
}

/// A roof with sketch openings (no shafts): an opening cuts slopes and landings, a dormer cuts slopes.
pub(crate) fn open_geometry(db: &Connection, id: i64, s: &RoofSpec) -> Result<Roof, Error> {
    let mut g = crate::roof_edge::cut(db, id, s)?;
    for o in crate::cuts::openings_of(db, "roof", id)? {
        let ring: Vec<ddc_geom::V2> = o.ring.iter().map(|p| v2(p.0, p.1)).collect();
        for p in ddc_geom::poly::convex_pieces(&ring) {
            g = cut_roof(&g, &p, o.dormer);
        }
    }
    Ok(g)
}

pub(crate) fn write(db: &Connection, s: &RoofSpec, id: Option<i64>) -> Result<i64, Error> {
    let (kind, mh, md, e) = match &s.kind {
        RoofKind::Footprint { mansard, .. } => (
            "footprint",
            mansard.map(|m| m.0),
            mansard.and_then(|m| m.1),
            [None; 5],
        ),
        RoofKind::Extrusion { a, b, depth, .. } => (
            "extrusion",
            None,
            None,
            [Some(a.0), Some(a.1), Some(b.0), Some(b.1), Some(*depth)],
        ),
        RoofKind::Arrow {
            tail, head, rise, ..
        } => (
            "arrow",
            None,
            None,
            [
                Some(tail.0),
                Some(tail.1),
                Some(head.0),
                Some(head.1),
                Some(*rise),
            ],
        ),
    };
    db.execute(
        "INSERT OR REPLACE INTO roof(id,level,stype,kind,offset,overhang,fascia,mansard_h,mansard_deg,ax,ay,bx,by,depth) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![id, s.level, s.slab_type, kind, s.offset, s.overhang, s.fascia, mh, md, e[0], e[1], e[2], e[3], e[4]],
    )?;
    let id = db.last_insert_rowid();
    db.execute("DELETE FROM roof_vertex WHERE roof=?1", [id])?;
    let rows: Vec<(f64, f64, Option<f64>)> = match &s.kind {
        RoofKind::Footprint { ring, slopes, .. } => ring
            .iter()
            .zip(slopes)
            .map(|(p, s)| (p.0, p.1, *s))
            .collect(),
        RoofKind::Extrusion { profile, .. } | RoofKind::Arrow { ring: profile, .. } => {
            profile.iter().map(|p| (p.0, p.1, None)).collect()
        }
    };
    for (i, r) in rows.iter().enumerate() {
        db.execute(
            "INSERT INTO roof_vertex VALUES (?1,?2,?3,?4,?5)",
            params![id, i as i64, r.0, r.1, r.2],
        )?;
    }
    Ok(id)
}

fn ids(db: &Connection, sql: &str, arg: i64) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare(sql)?;
    let r = st.query_map([arg], |r| r.get(0))?;
    Ok(r.collect::<Result<_, _>>()?)
}

/// Top profile of a straight wall along the roof underside: (u, h above the wall bottom).
fn wall_top(db: &Connection, wall: i64, roof: &Roof, t: f64) -> Result<Vec<(f64, f64)>, Error> {
    let (ax, ay, bx, by, base, bulge): (f64, f64, f64, f64, f64, f64) = db
        .query_row(
            "SELECT w.ax, w.ay, w.bx, w.by, l.elevation + w.base_offset, w.bulge FROM wall w \
             JOIN level l ON l.id = w.level WHERE w.id=?1",
            [wall],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .map_err(|_| Error::Invalid(format!("no such wall {wall}")))?;
    if bulge != 0.0 {
        return invalid(format!("wall {wall}: arc walls cannot attach to a roof"));
    }
    let p = roof.underside_along(v2(ax, ay), v2(bx, by), t);
    if p.len() < 2 {
        return invalid(format!("wall {wall} is not under the roof"));
    }
    let top: Vec<(f64, f64)> = p.iter().map(|q| (q.0, q.1 - base)).collect();
    if top.iter().any(|q| q.1 <= 0.0) {
        return invalid(format!("wall {wall}: roof underside below wall base"));
    }
    Ok(top)
}

/// Recomputes the profiles of walls attached to roof `id` (with snapshots for undo).
pub(crate) fn reattach(db: &Connection, id: i64, mut inv: Inv) -> Result<Inv, Error> {
    let walls = ids(db, "SELECT id FROM wall WHERE top_roof=?1 AND top_auto<>1 ORDER BY id", id)?;
    if walls.is_empty() {
        return Ok(inv);
    }
    let s = load(db, id)?;
    let roof = crate::roof_edge::shape(db, id, &s)?;
    let t: f64 = layers(db, s.slab_type)?.iter().map(|l| l.1).sum();
    for w in walls {
        let top = wall_top(db, w, &roof, t)?;
        inv = inv.restore(db, "wall", "id", w)?;
        db.execute(
            "UPDATE wall SET top_profile=?2 WHERE id=?1",
            params![w, crate::top_profile_text(&top)],
        )?;
    }
    Ok(inv)
}

/// Like [`reattach`], but a wall whose profile cannot be built from the new shape keeps its old
/// one; for roofs joined to the edited one (editing the target is not blocked by other walls).
pub(crate) fn reattach_soft(db: &Connection, id: i64, mut inv: Inv) -> Result<Inv, Error> {
    let walls = ids(db, "SELECT id FROM wall WHERE top_roof=?1 AND top_auto<>1 ORDER BY id", id)?;
    if walls.is_empty() {
        return Ok(inv);
    }
    let Ok(s) = load(db, id) else { return Ok(inv) };
    let Ok(roof) = crate::roof_edge::shape(db, id, &s) else {
        return Ok(inv);
    };
    let t: f64 = layers(db, s.slab_type)?.iter().map(|l| l.1).sum();
    for w in walls {
        let Ok(top) = wall_top(db, w, &roof, t) else {
            continue;
        };
        inv = inv.restore(db, "wall", "id", w)?;
        db.execute(
            "UPDATE wall SET top_profile=?2 WHERE id=?1",
            params![w, crate::top_profile_text(&top)],
        )?;
    }
    Ok(inv)
}

pub(crate) fn regenerate(db: &Connection) -> Result<(), Error> {
    db.execute("DELETE FROM roof_qty", [])?;
    for id in ids(db, "SELECT id FROM roof WHERE ?1=?1 ORDER BY id", 0)? {
        // a roof that cannot be built stays without quantities (shown in problems) and does not
        // block edits of other elements
        let Ok(s) = load(db, id) else { continue };
        let Ok(g) = cut_geometry(db, id, &s).or_else(|_| geometry(db, &s)) else {
            continue;
        };
        let q = g.quantities();
        for (pos, (mat, t)) in layers(db, s.slab_type)?.into_iter().enumerate() {
            db.execute(
                "INSERT INTO roof_qty VALUES (?1,?2,?3,?4,?5)",
                params![id, pos as i64, mat, q.area, q.area * t],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn issues(db: &Connection) -> Result<Vec<Issue>, Error> {
    let mut out = vec![];
    for id in ids(
        db,
        "SELECT id FROM roof WHERE id NOT IN (SELECT roof FROM roof_qty) AND ?1=?1 ORDER BY id",
        0,
    )? {
        out.push(Issue {
            table: "roof".into(),
            id,
            problem: "roof without quantities".into(),
        });
    }
    Ok(out)
}

impl Model {
    pub fn add_roof(&mut self, s: &RoofSpec) -> Result<i64, Error> {
        check(&self.db, s)?;
        self.op("roof_add", |db| {
            let id = write(db, s, None)?;
            let inv = Inv::default()
                .delete("roof_vertex", "roof", id)
                .delete("roof", "id", id);
            let inv = crate::roof_auto::sync(db, inv, None)?;
            Ok((id, id, inv))
        })
    }

    /// New roof shape/parameters; attached walls are recomputed in the same operation.
    pub fn update_roof(&mut self, id: i64, s: &RoofSpec) -> Result<(), Error> {
        check(&self.db, s)?;
        self.op("roof_edit", |db| {
            let inv = Inv::default()
                .delete("roof_vertex", "roof", id)
                .restore(db, "roof", "id", id)?
                .restore(db, "roof_vertex", "roof", id)?;
            if db.query_row("SELECT count(*) FROM roof WHERE id=?1", [id], |r| {
                r.get::<_, i64>(0)
            })? == 0
            {
                return invalid(format!("no such roof {id}"));
            }
            let old = load(db, id)?;
            write(db, s, Some(id))?;
            let inv = crate::roof_edge::follow_roof(db, inv, id, &old, s)?;
            let inv = reattach(db, id, inv)?;
            let inv = crate::roof_edge::reattach_dependents(db, inv, id)?;
            let inv = crate::roof_auto::sync(db, inv, None)?;
            Ok(((), id, inv))
        })
    }

    /// Roof deletion: attached walls keep their top profile and the attachment is removed.
    pub fn delete_roof(&mut self, id: i64) -> Result<(), Error> {
        self.op("roof_delete", |db| {
            let mut inv = Inv::default().restore(db, "roof", "id", id)?.restore(
                db,
                "roof_vertex",
                "roof",
                id,
            )?;
            for w in ids(db, "SELECT id FROM wall WHERE top_roof=?1", id)? {
                inv = inv.restore(db, "wall", "id", w)?;
            }
            if db.execute("DELETE FROM roof WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such roof {id}"));
            }
            db.execute("DELETE FROM roof_vertex WHERE roof=?1", [id])?;
            db.execute("UPDATE wall SET top_roof=NULL WHERE top_roof=?1", [id])?;
            let inv = crate::roof_edge::drop_roof(db, inv, id)?;
            let inv = crate::sketch::drop_owned(db, inv, "roof", id)?;
            let inv = crate::roof_auto::sync(db, inv, None)?;
            Ok(((), id, inv))
        })
    }

    pub fn roof(&self, id: i64) -> Result<RoofSpec, Error> {
        load(&self.db, id)
    }

    /// Roof geometry with cutouts (openings, dormers, shafts).
    pub fn roof_geometry(&self, id: i64) -> Result<Roof, Error> {
        cut_geometry(&self.db, id, &load(&self.db, id)?)
    }

    /// Roof solid of full layer thickness.
    pub fn roof_mesh(&self, id: i64) -> Result<Mesh, Error> {
        mesh_db(&self.db, id)
    }

    pub fn roof_quantities(&self, id: i64) -> Result<RoofQuantities, Error> {
        let s = load(&self.db, id)?;
        let g = cut_geometry(&self.db, id, &s)?.quantities();
        let mut st = self.db.prepare(
            "SELECT q.material, l.thickness, q.area, q.volume FROM roof_qty q JOIN roof r ON r.id = q.roof \
             JOIN slab_type_layer l ON l.stype = r.stype AND l.pos = q.pos WHERE q.roof=?1 ORDER BY q.pos",
        )?;
        let ls: Vec<(String, f64, f64, f64)> = st
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        Ok(RoofQuantities {
            thickness: ls.iter().map(|l| l.1).sum(),
            // the fascia profile already counts its own area
            fascia_area: if crate::roof_edge::has_fascia_sweep(&self.db, id)? {
                0.0
            } else {
                g.eave * s.fascia
            },
            geom: g,
            layers: ls,
        })
    }

    /// Attaches the top of a straight wall to the roof underside: the top profile follows the roof underside.
    pub fn attach_wall_top(&mut self, wall: i64, roof: i64) -> Result<(), Error> {
        crate::wall_spline::refuse_piece(&self.db, wall)?;
        self.op("wall_attach", |db| {
            let s = load(db, roof)?;
            let g = crate::roof_edge::shape(db, roof, &s)?;
            let t: f64 = layers(db, s.slab_type)?.iter().map(|l| l.1).sum();
            let top = wall_top(db, wall, &g, t)?;
            // the top attaches to one target only: a roof displaces a slab and a level
            let inv = Inv::default()
                .restore(db, "wall", "id", wall)?
                .sql(
                    "DELETE FROM wall_attach WHERE wall=?1 AND side='top'",
                    vec![wall.into()],
                )
                .restore(db, "wall_attach", "wall", wall)?
                .restore(db, "wall_attach_level", "wall", wall)?;
            db.execute(
                "DELETE FROM wall_attach WHERE wall=?1 AND side='top'",
                [wall],
            )?;
            db.execute("DELETE FROM wall_attach_level WHERE wall=?1", [wall])?;
            db.execute(
                "UPDATE wall SET top_profile=?2, top_roof=?3, top_auto=0, top_core=NULL WHERE id=?1",
                params![wall, crate::top_profile_text(&top), roof],
            )?;
            Ok(((), wall, inv))
        })
    }

    /// Removes the attachment: the top is flat again at `height`.
    pub fn detach_wall_top(&mut self, wall: i64) -> Result<(), Error> {
        self.op("wall_detach", |db| {
            let inv = Inv::default().restore(db, "wall", "id", wall)?;
            if db.execute(
                "UPDATE wall SET top_profile=NULL, top_roof=NULL, top_auto=2, top_core=NULL WHERE id=?1",
                [wall],
            )? == 0
            {
                return invalid(format!("no such wall {wall}"));
            }
            Ok(((), wall, inv))
        })
    }

    /// Top of the wall core under the roof with finish below the load-bearing layer; None means same as the wall top.
    pub fn wall_top_core(&self, wall: i64) -> Result<Option<Vec<(f64, f64)>>, Error> {
        let t: Option<String> = self.db.query_row("SELECT top_core FROM wall WHERE id=?1", [wall], |r| r.get(0))?;
        Ok(t.as_deref().and_then(crate::walls::parse_top_profile))
    }

    /// Wall type layers treated as the core (by flag or by the load-bearing layer rule).
    pub fn wall_core_layers(&self, wtype: i64) -> Result<Vec<bool>, Error> {
        crate::layer_core::wall_core(&self.db, wtype)
    }

    pub fn wall_top_roof(&self, wall: i64) -> Result<Option<i64>, Error> {
        Ok(self
            .db
            .query_row("SELECT top_roof FROM wall WHERE id=?1", [wall], |r| {
                r.get(0)
            })?)
    }
}

/// Roof solid of full layer thickness (for geometry joining).
pub(crate) fn mesh_db(db: &Connection, id: i64) -> Result<Mesh, Error> {
    let s = load(db, id)?;
    let t: f64 = layers(db, s.slab_type)?.iter().map(|l| l.1).sum();
    Ok(cut_geometry(db, id, &s)?.mesh(t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, WallSpec};

    fn wall_vol(m: &Model, w: i64) -> f64 {
        m.sql()
            .query_row("SELECT sum(volume) FROM wall_qty WHERE wall=?1", [w], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn near(a: f64, b: f64, what: &str) {
        assert!((a - b).abs() < 1e-9, "{what}: {a} vs {b}");
    }

    fn setup() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L0", 0.0).unwrap();
        let st = m
            .add_slab_type(
                "Roof",
                &[
                    LayerSpec {
                        material: "Tiles".into(),
                        thickness: 0.05,
                    },
                    LayerSpec {
                        material: "Rafters".into(),
                        thickness: 0.15,
                    },
                ],
            )
            .unwrap();
        (m, l, st)
    }

    fn rect() -> Vec<(f64, f64)> {
        vec![(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)]
    }

    #[test]
    fn hip_roof_quantities_layers_and_undo() {
        let (mut m, l, st) = setup();
        let s = RoofSpec {
            level: l,
            slab_type: st,
            offset: 3.0,
            overhang: 0.0,
            fascia: 0.2,
            kind: RoofKind::Footprint {
                ring: rect(),
                slopes: vec![Some(30.0); 4],
                mansard: None,
            },
        };
        let id = m.add_roof(&s).unwrap();
        let q = m.roof_quantities(id).unwrap();
        let a = 60.0 / 30f64.to_radians().cos();
        near(q.geom.area, a, "area");
        near(q.geom.ridge, 4.0, "ridge");
        near(q.layers[0].3, a * 0.05, "tiles");
        near(q.layers[1].3, a * 0.15, "rafters");
        near(q.fascia_area, 32.0 * 0.2, "fascia");
        assert!(m.roof_mesh(id).unwrap().is_watertight());
        assert_eq!(m.roof(id).unwrap(), s);
        m.assert_integrity();
        let mut g = s.clone();
        g.kind = RoofKind::Footprint {
            ring: rect(),
            slopes: vec![Some(30.0), None, Some(30.0), None],
            mansard: None,
        };
        m.update_roof(id, &g).unwrap();
        near(
            m.roof_quantities(id).unwrap().geom.ridge,
            10.0,
            "gable ridge",
        );
        m.undo().unwrap();
        assert_eq!(m.roof(id).unwrap(), s);
        assert!(m.delete_slab_type(st).is_err());
        m.delete_roof(id).unwrap();
        m.assert_integrity();
        m.undo().unwrap();
        m.assert_integrity();
        let bad = RoofSpec {
            kind: RoofKind::Footprint {
                ring: rect(),
                slopes: vec![None; 4],
                mansard: None,
            },
            ..s
        };
        assert!(m.add_roof(&bad).is_err());
    }

    #[test]
    fn gable_wall_attaches_to_roof_underside() {
        let (mut m, l, st) = setup();
        let wt = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (0.0, 6.0),
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        let r = m
            .add_roof(&RoofSpec {
                level: l,
                slab_type: st,
                offset: 3.0,
                overhang: 0.0,
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: rect(),
                    slopes: vec![Some(45.0), None, Some(45.0), None],
                    mansard: None,
                },
            })
            .unwrap();
        m.attach_wall_top(w, r).unwrap();
        let dz = 0.2 * 2f64.sqrt();
        let top = m.wall_top_profile(w).unwrap().unwrap();
        assert_eq!(top.len(), 3);
        near(top[0].1, 3.0 - dz, "eave");
        near(top[1].0, 3.0, "ridge u");
        near(top[1].1, 6.0 - dz, "ridge");
        // gable wall 6 × 0.2: a rectangle 3 − dz plus a triangle 6 × 3 / 2
        let v: f64 = wall_vol(&m, w);
        let plan = 6.0 * 0.2; // centred axis, ends without joints
        assert!((v - plan * (3.0 - dz + 1.5)).abs() < 0.2 * 0.2 * 3.0, "{v}");
        assert_eq!(m.wall_top_roof(w).unwrap(), Some(r));
        // a roof edit (30° slope) recomputes the wall in the same operation
        let mut s = m.roof(r).unwrap();
        s.kind = RoofKind::Footprint {
            ring: rect(),
            slopes: vec![Some(30.0), None, Some(30.0), None],
            mansard: None,
        };
        m.update_roof(r, &s).unwrap();
        let t30 = 30f64.to_radians().tan();
        let top = m.wall_top_profile(w).unwrap().unwrap();
        near(
            top[1].1,
            3.0 + 3.0 * t30 - 0.2 / 30f64.to_radians().cos(),
            "ridge 30",
        );
        m.undo().unwrap();
        near(
            m.wall_top_profile(w).unwrap().unwrap()[1].1,
            6.0 - dz,
            "undo restores wall",
        );
        m.delete_roof(r).unwrap();
        assert_eq!(m.wall_top_roof(w).unwrap(), None);
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(m.wall_top_roof(w).unwrap(), Some(r));
        m.detach_wall_top(w).unwrap();
        assert_eq!(m.wall_top_profile(w).unwrap(), None);
        m.assert_integrity();
    }

    #[test]
    fn extrusion_roof_and_migration_from_12() {
        let (mut m, l, st) = setup();
        let id = m
            .add_roof(&RoofSpec {
                level: l,
                slab_type: st,
                offset: 3.0,
                overhang: 0.0,
                fascia: 0.0,
                kind: RoofKind::Extrusion {
                    a: (0.0, 0.0),
                    b: (8.0, 0.0),
                    profile: vec![(0.0, 0.0), (4.0, 2.0), (8.0, 0.0)],
                    depth: 10.0,
                },
            })
            .unwrap();
        let q = m.roof_quantities(id).unwrap();
        near(q.geom.area, 20.0 * 20f64.sqrt(), "area");
        near(q.layers[1].3, 20.0 * 20f64.sqrt() * 0.15, "layer volume");
        // a schema 12 file (without the new tables and wall.top_roof) opens additively
        let path = std::env::temp_dir().join(format!("ddc_t79_{}.ddc", std::process::id()));
        let p = path.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&p);
        {
            let mut v = Model::open(&p).unwrap();
            v.add_level("A", 0.0).unwrap();
            v.sql()
                .execute_batch(
                    "DROP TABLE col; DROP TABLE beam; DROP TABLE member_type; DROP TABLE member_qty;
                     DROP TABLE roof; DROP TABLE roof_vertex; DROP TABLE roof_qty;
                     ALTER TABLE wall DROP COLUMN top_roof;
                     UPDATE meta SET value=12 WHERE key='schema_version';",
                )
                .unwrap();
        }
        let v = Model::open(&p).unwrap();
        assert_eq!(v.schema_version().unwrap(), crate::schema::VERSION);
        assert!(v.check_integrity().unwrap().is_empty());
        assert_eq!(v.level_list().unwrap().len(), 1);
        drop(v);
        let _ = std::fs::remove_file(&p);
    }
}
