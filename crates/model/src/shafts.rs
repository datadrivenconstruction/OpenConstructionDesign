// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Shafts, ceilings, slab openings (schema 14).
//! A shaft is a vertical cutout by an outline over an elevation range: it is subtracted from the
//! areas and volumes of slabs, ceilings, roofs (in plan, corrected for slope) and beams (by axis
//! length inside the outline). `shaft_cut` is derived: what each shaft cuts and by how much.

use ddc_geom::{poly, v2, V2};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

#[derive(Clone, Debug, PartialEq)]
pub struct ShaftSpec {
    pub level: i64,
    pub top_level: i64,
    pub base_offset: f64,
    pub top_offset: f64,
    pub ring: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CeilingSpec {
    pub level: i64,
    /// Slab type: layers from top to bottom.
    pub slab_type: i64,
    /// Ceiling bottom above the level.
    pub height: f64,
    pub outer: Vec<(f64, f64)>,
    pub holes: Vec<Vec<(f64, f64)>>,
}

/// A shaft cutout in an element.
#[derive(Clone, Debug, PartialEq)]
pub struct ShaftCut {
    pub kind: String,
    pub id: i64,
    pub area: f64,
    pub volume: f64,
}

fn invalid<T>(m: String) -> Result<T, Error> {
    Err(Error::Invalid(m))
}

fn elev(db: &Connection, l: i64) -> Result<f64, Error> {
    db.query_row("SELECT elevation FROM level WHERE id=?1", [l], |r| r.get(0))
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no level {l}")))
}

fn ccw(r: &[(f64, f64)]) -> Vec<V2> {
    let mut v: Vec<V2> = r.iter().map(|p| v2(p.0, p.1)).collect();
    if poly::area(&v) < 0.0 {
        v.reverse();
    }
    v
}

/// Shafts: (id, counter-clockwise outline, convex pieces, z0, z1).
type Shaft = (i64, Vec<V2>, Vec<Vec<V2>>, f64, f64);

fn shafts(db: &Connection) -> Result<Vec<Shaft>, Error> {
    let mut st = db.prepare(
        "SELECT s.id, b.elevation + s.base_offset, t.elevation + s.top_offset FROM shaft s \
         JOIN level b ON b.id=s.level JOIN level t ON t.id=s.top_level ORDER BY s.id",
    )?;
    let rows: Vec<(i64, f64, f64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let mut out = vec![];
    for (id, z0, z1) in rows {
        let ring = ccw(&ring_of(db, id)?);
        let pieces = poly::convex_pieces(&ring);
        out.push((id, ring, pieces, z0, z1));
    }
    Ok(out)
}

/// Shafts: (outline, bottom, top), a deduction for voids (the shaft is already removed from slabs).
pub(crate) fn prisms(db: &Connection) -> Result<Vec<(Vec<V2>, f64, f64)>, Error> {
    if !has_shafts(db)? {
        return Ok(vec![]);
    }
    Ok(shafts(db)?.into_iter().map(|s| (s.1, s.3, s.4)).collect())
}

pub(crate) fn ring_of(db: &Connection, id: i64) -> Result<Vec<(f64, f64)>, Error> {
    let mut st = db.prepare("SELECT x, y FROM shaft_vertex WHERE shaft=?1 ORDER BY idx")?;
    let r = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(r.collect::<Result<_, _>>()?)
}

/// Area of the region `rings` (outer + holes) cut by shafts intersecting (z0, z1):
/// (shaft, area).
pub(crate) fn cut_areas(
    db: &Connection,
    rings: &[Vec<(f64, f64)>],
    z0: f64,
    z1: f64,
) -> Result<Vec<(i64, f64)>, Error> {
    if !has_shafts(db)? {
        return Ok(vec![]);
    }
    let rs: Vec<Vec<V2>> = rings
        .iter()
        .map(|r| r.iter().map(|p| v2(p.0, p.1)).collect())
        .collect();
    let mut out = vec![];
    for (id, _, pieces, s0, s1) in shafts(db)? {
        if s1 <= z0 + 1e-9 || s0 >= z1 - 1e-9 {
            continue;
        }
        // signed rings: holes (clockwise) subtract themselves
        let a: f64 = rs.iter().map(|r| poly::overlap_area(r, &pieces)).sum();
        if a > 1e-12 {
            out.push((id, a));
        }
    }
    Ok(out)
}

/// Shafts for roof cutouts: (id, convex pieces, z0, z1).
pub(crate) fn shaft_pieces(db: &Connection) -> Result<Vec<(i64, Vec<Vec<V2>>, f64, f64)>, Error> {
    if !has_shafts(db)? {
        return Ok(vec![]);
    }
    Ok(shafts(db)?
        .into_iter()
        .map(|(id, _, p, z0, z1)| (id, p, z0, z1))
        .collect())
}

fn has_shafts(db: &Connection) -> Result<bool, Error> {
    Ok(db.query_row("SELECT count(*) FROM shaft", [], |r| r.get::<_, i64>(0))? > 0)
}

/// Outlines of shafts (counter-clockwise) that intersect (z0, z1) in height.
pub(crate) fn shaft_rings(db: &Connection, z0: f64, z1: f64) -> Result<Vec<(i64, Vec<V2>)>, Error> {
    if !has_shafts(db)? {
        return Ok(vec![]);
    }
    Ok(shafts(db)?
        .into_iter()
        .filter(|s| !(s.4 <= z0 + 1e-9 || s.3 >= z1 - 1e-9))
        .map(|s| (s.0, s.1))
        .collect())
}

fn layers(db: &Connection, stype: i64) -> Result<Vec<(String, f64)>, Error> {
    let mut st =
        db.prepare("SELECT material, thickness FROM slab_type_layer WHERE stype=?1 ORDER BY pos")?;
    let r = st.query_map([stype], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(r.collect::<Result<_, _>>()?)
}

pub(crate) fn ceiling_rings(db: &Connection, id: i64) -> Result<Vec<Vec<(f64, f64)>>, Error> {
    let mut st =
        db.prepare("SELECT ring, x, y FROM ceiling_vertex WHERE ceiling=?1 ORDER BY ring, idx")?;
    let rows = st.query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?)))?;
    let mut out: Vec<Vec<(f64, f64)>> = vec![];
    for row in rows {
        let (ring, x, y) = row?;
        while out.len() <= ring as usize {
            out.push(vec![]);
        }
        out[ring as usize].push((x, y));
    }
    Ok(out)
}

/// Ceilings: (id, stype, bottom).
pub(crate) fn ceilings(db: &Connection) -> Result<Vec<(i64, i64, f64)>, Error> {
    let mut st = db.prepare(
        "SELECT c.id, c.stype, l.elevation + c.height FROM ceiling c JOIN level l ON l.id=c.level ORDER BY c.id",
    )?;
    let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(r.collect::<Result<_, _>>()?)
}

/// Recomputes `ceiling_qty`, `shaft_cut` and the shaft deductions from roofs and beams (after
/// slabs, structures and roofs, which recreate their own quantities).
pub(crate) fn regenerate(db: &Connection) -> Result<(), Error> {
    db.execute("DELETE FROM ceiling_qty", [])?;
    db.execute("DELETE FROM shaft_cut", [])?;
    let cut = |s: i64, kind: &str, r: i64, a: f64, v: f64| -> Result<(), Error> {
        db.execute(
            "INSERT OR REPLACE INTO shaft_cut VALUES (?1,?2,?3,?4,?5)",
            params![s, kind, r, a, v],
        )?;
        Ok(())
    };
    // ceilings
    for (id, stype, bottom) in ceilings(db)? {
        let rs = ceiling_rings(db, id)?;
        let Some((outer, holes)) = rs.split_first() else {
            continue;
        };
        // a spike saved before they were refused still counts
        if crate::slabs::saved_profile(outer, holes).is_err() {
            continue;
        }
        let ls = layers(db, stype)?;
        let t: f64 = ls.iter().map(|l| l.1).sum();
        let cuts = cut_areas(db, &rs, bottom, bottom + t)?;
        // area is taken in plan after the cutouts (shafts and openings merged, no double deduction)
        let area = match crate::cuts::ceiling_cut(db, id)? {
            Some(c) => c.area(),
            None => continue,
        };
        for (sid, a) in cuts {
            cut(sid, "ceiling", id, a, a * t)?;
        }
        for (pos, (mat, th)) in ls.into_iter().enumerate() {
            db.execute(
                "INSERT INTO ceiling_qty VALUES (?1,?2,?3,?4,?5)",
                params![id, pos as i64, mat, area, area * th],
            )?;
        }
    }
    if !has_shafts(db)? {
        return Ok(());
    }
    // slabs: the deduction is already done in slab_qty, here only the bookkeeping
    let slabs: Vec<(i64, i64, f64)> = {
        let mut st = db.prepare(
            "SELECT s.id, s.stype, l.elevation + s.offset FROM slab s JOIN level l ON l.id=s.level ORDER BY s.id",
        )?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    for (id, stype, top) in slabs {
        let t: f64 = layers(db, stype)?.iter().map(|l| l.1).sum();
        let rs = crate::slabs::rings(db, id)?;
        for (sid, a) in cut_areas(db, &rs, top - t, top)? {
            cut(sid, "slab", id, a, a * t)?;
        }
    }
    // roofs: the solid is already cut (roofs::cut_geometry), here the bookkeeping is by the difference of true areas
    let roofs: Vec<i64> = {
        let mut st = db.prepare("SELECT id FROM roof ORDER BY id")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        v
    };
    for id in roofs {
        let s = crate::roofs::load(db, id)?;
        let g = crate::roofs::open_geometry(db, id, &s)?;
        let t: f64 = layers(db, s.slab_type)?.iter().map(|l| l.1).sum();
        let a0 = g.quantities().area;
        for (sid, _, pieces, z0, z1) in shafts(db)? {
            if !crate::roofs::shaft_hits(&g, &pieces, t, z0, z1) {
                continue;
            }
            let mut c = g.clone();
            for p in &pieces {
                c = ddc_geom::shape_edit::cut_roof(&c, p, false);
            }
            let a = a0 - c.quantities().area;
            if a > 1e-9 {
                cut(sid, "roof", id, a, a * t)?;
            }
        }
    }
    // beams: the fraction of the axis inside the shaft outline
    let beams: Vec<(i64, [f64; 6], f64)> = {
        let mut st = db.prepare(
            "SELECT b.id, b.ax, b.ay, b.bx, b.by, l.elevation + b.az, l.elevation + b.bz, b.bulge \
             FROM beam b JOIN level l ON l.id=b.level ORDER BY b.id",
        )?;
        let v = st
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    [
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ],
                    r.get(7)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        v
    };
    let all = shafts(db)?;
    for (id, e, bulge) in beams {
        if bulge != 0.0 {
            continue;
        }
        let (a, b) = (v2(e[0], e[1]), v2(e[2], e[3]));
        let z = (e[4] + e[5]) / 2.0;
        let mut frac = 0.0;
        let mut hits = vec![];
        for (sid, ring, _, s0, s1) in &all {
            if z <= *s0 || z >= *s1 {
                continue;
            }
            let f: f64 = poly::inside_intervals(a, b, &[ring.clone()])
                .iter()
                .map(|iv| iv.1 - iv.0)
                .sum();
            if f > 1e-12 {
                frac += f;
                hits.push((*sid, f));
            }
        }
        if hits.is_empty() {
            continue;
        }
        let frac = frac.min(1.0);
        let vol: Option<f64> = db
            .query_row(
                "SELECT volume FROM member_qty WHERE kind='beam' AND ref=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(vol) = vol else { continue };
        db.execute(
            "UPDATE member_qty SET volume = volume - ?2, cut = cut + ?2 WHERE kind='beam' AND ref=?1",
            params![id, vol * frac],
        )?;
        let len = (b - a).len();
        for (sid, f) in hits {
            cut(sid, "beam", id, f * len, vol * f)?;
        }
    }
    Ok(())
}

/// `id`: the shaft being edited (or a copy's source), whose saved spikes never block it.
fn check_shaft(db: &Connection, s: &ShaftSpec, id: Option<i64>) -> Result<(), Error> {
    let z0 = elev(db, s.level)? + s.base_offset;
    let z1 = elev(db, s.top_level)? + s.top_offset;
    if !(z0.is_finite() && z1.is_finite() && z1 > z0) {
        return invalid("shaft: top must be above base".into());
    }
    let old = match id {
        Some(i) => vec![ring_of(db, i)?],
        None => vec![],
    };
    crate::slabs::profile_keeping(&s.ring, &[], &old)?;
    Ok(())
}

/// Plan transform of a shaft outline (move, rotate, mirror): the outline is rewritten in
/// counter-clockwise order, levels and offsets stay. Cuts follow by the regeneration.
pub(crate) fn transform(
    db: &Connection,
    id: i64,
    f: &dyn Fn((f64, f64)) -> (f64, f64),
) -> Result<(), Error> {
    let ring: Vec<(f64, f64)> = ring_of(db, id)?.into_iter().map(f).collect();
    write_shaft_ring(db, id, &ring)
}

/// Shaft outline (counter-clockwise), without changing elevations.
pub(crate) fn write_shaft_ring(db: &Connection, id: i64, ring: &[(f64, f64)]) -> Result<(), Error> {
    db.execute("DELETE FROM shaft_vertex WHERE shaft=?1", [id])?;
    for (i, p) in ccw(ring).iter().enumerate() {
        db.execute(
            "INSERT INTO shaft_vertex VALUES (?1,?2,?3,?4)",
            params![id, i as i64, p.x, p.y],
        )?;
    }
    Ok(())
}

/// Ceiling outlines from a validated profile.
pub(crate) fn write_ceiling_rings(
    db: &Connection,
    id: i64,
    p: &ddc_geom::Profile,
) -> Result<(), Error> {
    db.execute("DELETE FROM ceiling_vertex WHERE ceiling=?1", [id])?;
    for (ring, pts) in p.rings().enumerate() {
        for (idx, q) in pts.iter().enumerate() {
            db.execute(
                "INSERT INTO ceiling_vertex VALUES (?1,?2,?3,?4,?5)",
                params![id, ring as i64, idx as i64, q.x, q.y],
            )?;
        }
    }
    Ok(())
}

/// Ceiling bottom and top.
pub(crate) fn ceiling_z(db: &Connection, id: i64) -> Result<(f64, f64), Error> {
    let (st, z0): (i64, f64) = db.query_row(
        "SELECT c.stype, l.elevation + c.height FROM ceiling c JOIN level l ON l.id=c.level WHERE c.id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let t: f64 = layers(db, st)?.iter().map(|l| l.1).sum();
    Ok((z0, z0 + t))
}

fn write_shaft(db: &Connection, s: &ShaftSpec, id: Option<i64>) -> Result<i64, Error> {
    db.execute(
        "INSERT OR REPLACE INTO shaft(id,level,top_level,base_offset,top_offset) VALUES (?1,?2,?3,?4,?5)",
        params![id, s.level, s.top_level, s.base_offset, s.top_offset],
    )?;
    let id = db.last_insert_rowid();
    db.execute("DELETE FROM shaft_vertex WHERE shaft=?1", [id])?;
    for (i, p) in ccw(&s.ring).iter().enumerate() {
        db.execute(
            "INSERT INTO shaft_vertex VALUES (?1,?2,?3,?4)",
            params![id, i as i64, p.x, p.y],
        )?;
    }
    Ok(id)
}

pub(crate) fn write_ceiling(
    db: &Connection,
    s: &CeilingSpec,
    id: Option<i64>,
) -> Result<i64, Error> {
    write_ceiling_from(db, s, id, id)
}

/// `keep`: the ceiling whose saved spikes the outline may carry (itself, or a copy's source).
fn write_ceiling_from(
    db: &Connection,
    s: &CeilingSpec,
    id: Option<i64>,
    keep: Option<i64>,
) -> Result<i64, Error> {
    let p = ceiling_profile(db, s, keep)?;
    db.execute(
        "INSERT OR REPLACE INTO ceiling(id,level,stype,height) VALUES (?1,?2,?3,?4)",
        params![id, s.level, s.slab_type, s.height],
    )?;
    let id = db.last_insert_rowid();
    db.execute("DELETE FROM ceiling_vertex WHERE ceiling=?1", [id])?;
    for (ring, pts) in p.rings().enumerate() {
        for (idx, q) in pts.iter().enumerate() {
            db.execute(
                "INSERT INTO ceiling_vertex VALUES (?1,?2,?3,?4,?5)",
                params![id, ring as i64, idx as i64, q.x, q.y],
            )?;
        }
    }
    Ok(id)
}

/// The outline of `s`; `id`: the ceiling being edited, whose saved spikes never block it.
fn ceiling_profile(
    db: &Connection,
    s: &CeilingSpec,
    id: Option<i64>,
) -> Result<ddc_geom::Profile, Error> {
    let old = match id {
        Some(i) => ceiling_rings(db, i)?,
        None => vec![],
    };
    crate::slabs::profile_keeping(&s.outer, &s.holes, &old)
}

pub(crate) fn check_ceiling(
    db: &Connection,
    s: &CeilingSpec,
    id: Option<i64>,
) -> Result<(), Error> {
    elev(db, s.level)?;
    if layers(db, s.slab_type)?.is_empty() {
        return invalid(format!("ceiling: slab type {} has no layers", s.slab_type));
    }
    if !s.height.is_finite() {
        return invalid("ceiling: height".into());
    }
    ceiling_profile(db, s, id)?;
    Ok(())
}

impl Model {
    pub fn add_shaft(&mut self, s: &ShaftSpec) -> Result<i64, Error> {
        self.add_shaft_keeping(s, None)
    }

    /// A copy of shaft `from` as `s` (moved, turned or mirrored): a spike saved in the source
    /// before they were refused comes along; a new one is refused as in `add_shaft`.
    pub fn add_shaft_copy(&mut self, from: i64, s: &ShaftSpec) -> Result<i64, Error> {
        self.add_shaft_keeping(s, Some(from))
    }

    fn add_shaft_keeping(&mut self, s: &ShaftSpec, keep: Option<i64>) -> Result<i64, Error> {
        check_shaft(&self.db, s, keep)?;
        self.op("shaft_add", |db| {
            let id = write_shaft(db, s, None)?;
            let inv = Inv::default()
                .delete("shaft_vertex", "shaft", id)
                .delete("shaft", "id", id);
            Ok((id, id, inv))
        })
    }

    pub fn update_shaft(&mut self, id: i64, s: &ShaftSpec) -> Result<(), Error> {
        check_shaft(&self.db, s, Some(id))?;
        self.op("shaft_edit", |db| {
            if db.query_row("SELECT count(*) FROM shaft WHERE id=?1", [id], |r| {
                r.get::<_, i64>(0)
            })? == 0
            {
                return invalid(format!("no such shaft {id}"));
            }
            let inv = Inv::default()
                .delete("shaft_vertex", "shaft", id)
                .restore(db, "shaft", "id", id)?
                .restore(db, "shaft_vertex", "shaft", id)?;
            write_shaft(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_shaft(&mut self, id: i64) -> Result<(), Error> {
        self.delete_ref("shaft", "shaft_delete", id, &[("shaft_vertex", "shaft")])
    }

    /// What a shaft cuts: per element in (kind, id) order.
    pub fn shaft_cuts(&self, id: i64) -> Result<Vec<ShaftCut>, Error> {
        let mut st = self.db.prepare(
            "SELECT kind, ref, area, volume FROM shaft_cut WHERE shaft=?1 ORDER BY kind, ref",
        )?;
        let r = st.query_map([id], |r| {
            Ok(ShaftCut {
                kind: r.get(0)?,
                id: r.get(1)?,
                area: r.get(2)?,
                volume: r.get(3)?,
            })
        })?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    pub fn shaft_ring(&self, id: i64) -> Result<Vec<(f64, f64)>, Error> {
        ring_of(&self.db, id)
    }

    pub fn shaft(&self, id: i64) -> Result<ShaftSpec, Error> {
        let (level, top_level, base_offset, top_offset) = self
            .db
            .query_row(
                "SELECT level, top_level, base_offset, top_offset FROM shaft WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no such shaft {id}")))?;
        Ok(ShaftSpec {
            level,
            top_level,
            base_offset,
            top_offset,
            ring: ring_of(&self.db, id)?,
        })
    }

    /// Shafts crossing the plan plane of level `level` (cut plane 1.2 m above the elevation).
    pub fn shafts_on_level(&self, level: i64) -> Result<Vec<i64>, Error> {
        let mut st = self.db.prepare(
            "SELECT s.id FROM shaft s JOIN level b ON b.id=s.level JOIN level t ON t.id=s.top_level              JOIN level l ON l.id=?1 WHERE b.elevation + s.base_offset < l.elevation + 1.2              AND t.elevation + s.top_offset > l.elevation ORDER BY s.id",
        )?;
        let r = st.query_map([level], |r| r.get(0))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    /// All project shafts in ascending id order.
    pub fn shaft_ids(&self) -> Result<Vec<i64>, Error> {
        let mut st = self.db.prepare("SELECT id FROM shaft ORDER BY id")?;
        let r = st.query_map([], |r| r.get(0))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    /// An opening in a slab (or a slab-roof): adds a hole, one undo.
    pub fn add_slab_opening(&mut self, slab: i64, ring: &[(f64, f64)]) -> Result<(), Error> {
        let rs = crate::slabs::rings(&self.db, slab)?;
        let Some((outer, holes)) = rs.split_first() else {
            return invalid(format!("no such slab {slab}"));
        };
        let mut holes = holes.to_vec();
        holes.push(ring.to_vec());
        let outer = outer.to_vec();
        self.set_slab_boundary(slab, &outer, &holes)
    }

    /// Removes hole `k` (0-based) of a slab.
    pub fn remove_slab_opening(&mut self, slab: i64, k: usize) -> Result<(), Error> {
        let rs = crate::slabs::rings(&self.db, slab)?;
        let Some((outer, holes)) = rs.split_first() else {
            return invalid(format!("no such slab {slab}"));
        };
        if k >= holes.len() {
            return invalid(format!("slab {slab} has no opening {k}"));
        }
        let mut holes = holes.to_vec();
        holes.remove(k);
        let outer = outer.to_vec();
        self.set_slab_boundary(slab, &outer, &holes)
    }

    pub fn add_ceiling(&mut self, s: &CeilingSpec) -> Result<i64, Error> {
        self.add_ceiling_keeping(s, None)
    }

    /// A copy of ceiling `from` as `s` (moved, turned or mirrored): a spike saved in the source
    /// before they were refused comes along; a new one is refused as in `add_ceiling`.
    pub fn add_ceiling_copy(&mut self, from: i64, s: &CeilingSpec) -> Result<i64, Error> {
        self.add_ceiling_keeping(s, Some(from))
    }

    fn add_ceiling_keeping(&mut self, s: &CeilingSpec, keep: Option<i64>) -> Result<i64, Error> {
        check_ceiling(&self.db, s, keep)?;
        self.op("ceiling_add", |db| {
            let id = write_ceiling_from(db, s, None, keep)?;
            let inv = Inv::default()
                .delete("ceiling_vertex", "ceiling", id)
                .delete("ceiling", "id", id);
            Ok((id, id, inv))
        })
    }

    pub fn update_ceiling(&mut self, id: i64, s: &CeilingSpec) -> Result<(), Error> {
        check_ceiling(&self.db, s, Some(id))?;
        self.op("ceiling_edit", |db| {
            if db.query_row("SELECT count(*) FROM ceiling WHERE id=?1", [id], |r| {
                r.get::<_, i64>(0)
            })? == 0
            {
                return invalid(format!("no such ceiling {id}"));
            }
            let inv = Inv::default()
                .delete("ceiling_vertex", "ceiling", id)
                .restore(db, "ceiling", "id", id)?
                .restore(db, "ceiling_vertex", "ceiling", id)?;
            write_ceiling(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    /// Deletes a ceiling with its outline and the components hung from it.
    pub fn delete_ceiling(&mut self, id: i64) -> Result<(), Error> {
        self.op("ceiling_delete", |db| {
            let inv = Inv::default().restore(db, "ceiling", "id", id)?.restore(
                db,
                "ceiling_vertex",
                "ceiling",
                id,
            )?;
            if db.execute("DELETE FROM ceiling WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such ceiling {id}"));
            }
            db.execute("DELETE FROM ceiling_vertex WHERE ceiling=?1", [id])?;
            let inv = crate::components::drop_hosted(db, inv, "host_ceiling", id)?;
            let inv = crate::sketch::drop_owned(db, inv, "ceiling", id)?;
            Ok(((), id, inv))
        })
    }

    /// Ceiling quantities per layer: (material, area, volume).
    pub fn ceiling_quantities(&self, id: i64) -> Result<Vec<(String, f64, f64)>, Error> {
        let mut st = self.db.prepare(
            "SELECT material, area, volume FROM ceiling_qty WHERE ceiling=?1 ORDER BY pos",
        )?;
        let r = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    pub fn ceiling_rings(&self, id: i64) -> Result<Vec<Vec<(f64, f64)>>, Error> {
        ceiling_rings(&self.db, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, SlabSpec};

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    }

    fn three_floors() -> (Model, Vec<i64>, Vec<i64>, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let t = m
            .add_slab_type(
                "floor",
                &[LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let mut lv = vec![];
        let mut sl = vec![];
        for k in 0..4 {
            lv.push(m.add_level(&format!("L{k}"), 3.0 * k as f64).unwrap());
        }
        for l in &lv[1..] {
            sl.push(
                m.add_slab(&SlabSpec {
                    level: *l,
                    slab_type: t,
                    offset: 0.0,
                    outer: rect(0., 0., 10., 8.),
                    holes: vec![],
                })
                .unwrap(),
            );
        }
        (m, lv, sl, t)
    }

    #[test]
    fn shaft_moves_copies_mirrors_and_undoes() {
        use crate::Target;
        let (mut m, lv, sl, _) = three_floors();
        let spec = ShaftSpec {
            level: lv[0],
            top_level: lv[3],
            base_offset: 0.0,
            top_offset: 0.5,
            ring: rect(2., 2., 4., 3.5),
        };
        let sh = m.add_shaft(&spec).unwrap();
        assert_eq!(m.shaft(sh).unwrap(), spec);
        assert_eq!(m.shafts_on_level(lv[1]).unwrap(), vec![sh]);
        let t = [Target::Shaft(sh)];
        // moved out of the 10 x 8 slab: the cutout disappears
        m.move_elements(&t, (20.0, 0.0)).unwrap();
        assert_eq!(m.shaft(sh).unwrap().ring[0], (22.0, 2.0));
        assert!((m.slab_quantities(sl[0]).unwrap()[0].area - 80.0).abs() < 1e-12);
        m.undo().unwrap();
        assert_eq!(m.shaft(sh).unwrap(), spec);
        assert!((m.slab_quantities(sl[0]).unwrap()[0].area - 77.0).abs() < 1e-12);
        let c = m.copy_elements(&t, &[(0.0, 3.0)]).unwrap();
        let Target::Shaft(n) = c[0] else {
            panic!("{c:?}")
        };
        assert_eq!(m.shaft_ids().unwrap(), vec![sh, n]);
        assert_eq!(m.shaft(n).unwrap().top_level, lv[3]);
        assert_eq!(m.shaft(n).unwrap().ring.len(), 4);
        m.undo().unwrap();
        assert_eq!(m.shaft_ids().unwrap(), vec![sh]);
        m.mirror_elements(&t, (0.0, 0.0), (0.0, 1.0), false).unwrap();
        let r = m.shaft(sh).unwrap().ring;
        let a2: f64 = (0..r.len())
            .map(|i| r[i].0 * r[(i + 1) % r.len()].1 - r[(i + 1) % r.len()].0 * r[i].1)
            .sum();
        assert!(a2 > 0.0, "ring stays counter-clockwise: {r:?}");
        assert!(r.iter().all(|p| p.0 < 0.0));
        m.undo().unwrap();
        m.rotate_elements(&t, (0.0, 0.0), std::f64::consts::PI, false)
            .unwrap();
        assert!(m.shaft(sh).unwrap().ring.iter().all(|p| p.0 < 0.0));
        m.undo().unwrap();
        assert_eq!(m.shaft(sh).unwrap(), spec);
        m.assert_integrity();
    }

    /// A riser ending at the attic floor stays under a hip roof (its overhang dips below that
    /// level, the roof above the shaft does not); one carried through the roof cuts it.
    #[test]
    fn shaft_cuts_a_hip_roof_only_where_it_reaches_it() {
        use crate::{RoofKind, RoofSpec};
        let (mut m, lv, _, t) = three_floors();
        let roof = m
            .add_roof(&RoofSpec {
                level: lv[3],
                slab_type: t,
                offset: 0.0,
                overhang: 0.5,
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: rect(0., 0., 10., 8.),
                    slopes: vec![Some(30.0); 4],
                    mansard: None,
                },
            })
            .unwrap();
        let mut spec = ShaftSpec {
            level: lv[2],
            top_level: lv[3],
            base_offset: 0.0,
            top_offset: 0.0,
            ring: rect(4., 3., 5., 4.),
        };
        let sh = m.add_shaft(&spec).unwrap();
        let roof_cuts = |m: &Model| {
            m.sql()
                .query_row(
                    "SELECT count(*) FROM shaft_cut WHERE kind='roof' AND ref=?1",
                    [roof],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
        };
        let whole = m.roof_quantities(roof).unwrap().geom.area;
        assert_eq!(roof_cuts(&m), 0);
        spec.top_offset = 3.0;
        m.update_shaft(sh, &spec).unwrap();
        assert_eq!(roof_cuts(&m), 1);
        assert!(m.roof_quantities(roof).unwrap().geom.area < whole - 0.5);
        m.assert_integrity();
    }

    #[test]
    fn shaft_cuts_three_slabs_and_undoes_in_one_step() {
        let (mut m, lv, sl, _) = three_floors();
        let ops = m
            .sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get::<_, i64>(0))
            .unwrap();
        let sh = m
            .add_shaft(&ShaftSpec {
                level: lv[0],
                top_level: lv[3],
                base_offset: 0.0,
                top_offset: 0.5,
                ring: rect(2., 2., 4., 3.5),
            })
            .unwrap();
        m.assert_integrity();
        for s in &sl {
            let q = m.slab_quantities(*s).unwrap();
            assert!((q[0].area - 77.0).abs() < 1e-12, "{q:?}");
            assert!((q[0].volume - 15.4).abs() < 1e-12);
        }
        let cuts = m.shaft_cuts(sh).unwrap();
        assert_eq!(cuts.len(), 3);
        assert!(cuts
            .iter()
            .all(|c| c.kind == "slab" && (c.area - 3.0).abs() < 1e-12));
        assert!((cuts.iter().map(|c| c.volume).sum::<f64>() - 1.8).abs() < 1e-12);
        m.undo().unwrap();
        let after = m
            .sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get::<_, i64>(0))
            .unwrap();
        assert_eq!(after, ops);
        for s in &sl {
            assert!((m.slab_quantities(*s).unwrap()[0].area - 80.0).abs() < 1e-12);
        }
        assert_eq!(
            m.sql()
                .query_row("SELECT count(*) FROM shaft_cut", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        // a shaft up to the second level cuts only one slab
        let sh = m
            .add_shaft(&ShaftSpec {
                level: lv[0],
                top_level: lv[1],
                base_offset: 0.0,
                top_offset: 0.0,
                ring: rect(2., 2., 4., 3.5),
            })
            .unwrap();
        // shaft top at the L1 slab top elevation: the layer [2.8, 3.0] is crossed
        assert_eq!(m.shaft_cuts(sh).unwrap().len(), 1);
        m.assert_integrity();
    }

    #[test]
    fn slab_opening_and_ceiling() {
        let (mut m, lv, sl, t) = three_floors();
        m.add_slab_opening(sl[0], &rect(1., 1., 2., 2.)).unwrap();
        assert!((m.slab_quantities(sl[0]).unwrap()[0].area - 79.0).abs() < 1e-12);
        m.undo().unwrap();
        assert!((m.slab_quantities(sl[0]).unwrap()[0].area - 80.0).abs() < 1e-12);
        let c = m
            .add_ceiling(&CeilingSpec {
                level: lv[0],
                slab_type: t,
                height: 2.6,
                outer: rect(0., 0., 5., 4.),
                holes: vec![],
            })
            .unwrap();
        let q = m.ceiling_quantities(c).unwrap();
        assert!((q[0].1 - 20.0).abs() < 1e-12 && (q[0].2 - 4.0).abs() < 1e-12);
        m.add_shaft(&ShaftSpec {
            level: lv[0],
            top_level: lv[3],
            base_offset: 0.0,
            top_offset: 0.0,
            ring: rect(4., 3., 6., 5.),
        })
        .unwrap();
        // shaft intersecting a 1×1 ceiling
        let q = m.ceiling_quantities(c).unwrap();
        assert!((q[0].1 - 19.0).abs() < 1e-12, "{q:?}");
        m.assert_integrity();
        assert!(m.delete_ceiling(c).is_ok());
        m.assert_integrity();
    }

    fn bbox(r: &[(f64, f64)]) -> (f64, f64, f64, f64) {
        r.iter().fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |a, p| {
            (a.0.min(p.0), a.1.min(p.1), a.2.max(p.0), a.3.max(p.1))
        })
    }

    #[test]
    fn shaft_plan_transforms_keep_the_cuts_consistent_with_every_slab() {
        use crate::Target;
        let (mut m, lv, sl, _) = three_floors();
        let sh = m
            .add_shaft(&ShaftSpec {
                level: lv[0],
                top_level: lv[3],
                base_offset: 0.0,
                top_offset: 0.5,
                ring: rect(2., 2., 4., 3.5),
            })
            .unwrap();
        let cut_area = |m: &Model, id: i64| -> f64 {
            m.shaft_cuts(id).unwrap().iter().map(|c| c.area).sum()
        };
        let slab_area = |m: &Model| -> Vec<f64> {
            sl.iter()
                .map(|s| m.slab_quantities(*s).unwrap()[0].area)
                .collect()
        };

        // inside the slabs: the same three cuts, the outline follows
        m.move_elements(&[Target::Shaft(sh)], (1.0, 0.5)).unwrap();
        assert_eq!(bbox(&m.shaft_ring(sh).unwrap()), (3.0, 2.5, 5.0, 4.0));
        assert_eq!(m.shaft_cuts(sh).unwrap().len(), 3);
        assert!((cut_area(&m, sh) - 9.0).abs() < 1e-9);
        assert!(slab_area(&m).iter().all(|a| (a - 77.0).abs() < 1e-9));

        // out of the slabs: no cut is left and the slabs are whole
        m.move_elements(&[Target::Shaft(sh)], (20.0, 0.0)).unwrap();
        assert!(m.shaft_cuts(sh).unwrap().is_empty());
        assert!(slab_area(&m).iter().all(|a| (a - 80.0).abs() < 1e-9));
        m.undo().unwrap();
        assert_eq!(m.shaft_cuts(sh).unwrap().len(), 3);
        assert!(slab_area(&m).iter().all(|a| (a - 77.0).abs() < 1e-9));

        // half turn about its centre and a mirror across a vertical line inside the slabs
        m.rotate_elements(&[Target::Shaft(sh)], (4.0, 3.25), std::f64::consts::PI, false)
            .unwrap();
        let b = bbox(&m.shaft_ring(sh).unwrap());
        assert!((b.0 - 3.0).abs() < 1e-9 && (b.3 - 4.0).abs() < 1e-9, "{b:?}");
        m.mirror_elements(&[Target::Shaft(sh)], (6.0, 0.0), (6.0, 1.0), false)
            .unwrap();
        let ring = m.shaft_ring(sh).unwrap();
        let b = bbox(&ring);
        assert!((b.0 - 7.0).abs() < 1e-9 && (b.2 - 9.0).abs() < 1e-9, "{b:?}");
        let pts: Vec<_> = ring.iter().map(|p| v2(p.0, p.1)).collect();
        assert!(poly::area(&pts) > 0.0, "the outline stays counter-clockwise");
        assert!((cut_area(&m, sh) - 9.0).abs() < 1e-9);
        m.assert_integrity();
    }

    #[test]
    fn shaft_copy_and_array_cut_the_slabs_once_each_and_undo_whole() {
        use crate::Target;
        let (mut m, lv, sl, _) = three_floors();
        let sh = m
            .add_shaft(&ShaftSpec {
                level: lv[0],
                top_level: lv[3],
                base_offset: 0.0,
                top_offset: 0.5,
                ring: rect(1., 1., 2., 2.),
            })
            .unwrap();
        let made = m
            .copy_elements(&[Target::Shaft(sh)], &[(0.0, 4.0)])
            .unwrap();
        let Target::Shaft(n) = made[0] else { panic!() };
        assert_ne!(n, sh);
        assert_eq!(m.shaft(n).unwrap().top_offset, 0.5);
        assert_eq!(bbox(&m.shaft_ring(n).unwrap()), (1.0, 5.0, 2.0, 6.0));
        assert_eq!(m.shaft_cuts(n).unwrap().len(), 3);
        assert!((m.slab_quantities(sl[0]).unwrap()[0].area - 78.0).abs() < 1e-9);
        m.undo().unwrap();
        assert!(m.shaft(n).is_err());
        assert!((m.slab_quantities(sl[0]).unwrap()[0].area - 79.0).abs() < 1e-9);

        let row = m
            .array_linear(&[Target::Shaft(sh)], 3, crate::Spacing::Step((2.0, 0.0)))
            .unwrap();
        assert_eq!(row.len(), 2);
        assert!((m.slab_quantities(sl[0]).unwrap()[0].area - 77.0).abs() < 1e-9);
        m.undo().unwrap();
        assert_eq!(m.shaft_ring(sh).unwrap().len(), 4);
        assert!((m.slab_quantities(sl[0]).unwrap()[0].area - 79.0).abs() < 1e-9);
        m.assert_integrity();
    }

    #[test]
    fn shaft_spec_reads_back_and_lists_by_level() {
        let (mut m, lv, _, _) = three_floors();
        let s = ShaftSpec {
            level: lv[0],
            top_level: lv[2],
            base_offset: 0.0,
            top_offset: 0.25,
            ring: rect(2., 2., 4., 3.5),
        };
        let sh = m.add_shaft(&s).unwrap();
        let got = m.shaft(sh).unwrap();
        assert_eq!((got.level, got.top_level, got.top_offset), (s.level, s.top_level, 0.25));
        assert_eq!(bbox(&got.ring), (2.0, 2.0, 4.0, 3.5));
        assert!(m.shaft(sh + 100).is_err());
        assert_eq!(m.shafts_on_level(lv[1]).unwrap(), vec![sh]);
        assert!(m.shafts_on_level(lv[3]).unwrap().is_empty());
    }
}
