// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Slabs: types (layers from top to bottom), instances with an outline and holes, quantities.
//! Slab top = level elevation + `offset`; layer volume = outline area × thickness (exact).
//! `slab_qty` is derived: recomputed in the same transaction as the edit.
//! Slope (`slab.slope`): the top is a plane through a point at elevation offset; thickness is
//! vertical, so the volume remains exactly area × thickness.

use ddc_geom::slope::Plane;
use ddc_geom::{v2, Profile, Tol};
use rusqlite::{params, Connection};

use crate::ops::Inv;
use crate::{Error, LayerSpec, Model};

#[derive(Clone, Debug, PartialEq)]
pub struct SlabSpec {
    pub level: i64,
    pub slab_type: i64,
    /// Top relative to the level.
    pub offset: f64,
    pub outer: Vec<(f64, f64)>,
    pub holes: Vec<Vec<(f64, f64)>>,
}

/// Quantity of a slab layer.
#[derive(Clone, Debug, PartialEq)]
pub struct SlabQty {
    pub material: String,
    pub thickness: f64,
    /// Plan area (projection).
    pub area: f64,
    /// Area of the sloped top/bottom face: plan × √(1 + gx² + gy²); equals `area` for a flat slab.
    pub surface_area: f64,
    pub volume: f64,
}

fn invalid<T>(msg: String) -> Result<T, Error> {
    Err(Error::Invalid(msg))
}

fn count(db: &Connection, sql: &str, id: i64) -> Result<i64, Error> {
    Ok(db.query_row(sql, [id], |r| r.get(0))?)
}

/// A validated profile (orientation normalised); an error names the outline and edges.
pub(crate) fn profile(outer: &[(f64, f64)], holes: &[Vec<(f64, f64)>]) -> Result<Profile, Error> {
    let ring = |r: &[(f64, f64)]| r.iter().map(|p| v2(p.0, p.1)).collect::<Vec<_>>();
    Profile::new(
        ring(outer),
        holes.iter().map(|h| ring(h)).collect(),
        Tol::default(),
    )
    .map_err(|e| Error::Geom(format!("slab boundary: {e:?}")))
}

fn rings_v2(rs: &[Vec<(f64, f64)>]) -> Vec<Vec<ddc_geom::V2>> {
    rs.iter()
        .map(|r| r.iter().map(|p| v2(p.0, p.1)).collect())
        .collect()
}

/// As `profile` for an outline read back from the model: a spike saved before they were
/// refused is kept, so the element still draws, counts and opens.
pub(crate) fn saved_profile(
    outer: &[(f64, f64)],
    holes: &[Vec<(f64, f64)>],
) -> Result<Profile, Error> {
    let mut it = rings_v2(&[outer.to_vec()]).into_iter().chain(rings_v2(holes));
    let o = it.next().unwrap_or_default();
    Profile::new_keeping_spikes(o, it.collect(), Tol::default())
        .map_err(|e| Error::Geom(format!("slab boundary: {e:?}")))
}

/// A spike: where its tip is (ring, vertex), the tip, and the lengths of its two edges,
/// shorter first (kept by a move, turn or mirror).
type Spike = ((usize, usize), ddc_geom::V2, f64, f64);

/// Spikes of all rings (`ddc_geom::profile::ring_spikes`).
fn spikes(rs: &[Vec<ddc_geom::V2>]) -> Vec<Spike> {
    let tol = Tol::default();
    rs.iter()
        .enumerate()
        .flat_map(|(k, r)| {
            let n = r.len();
            ddc_geom::profile::ring_spikes(r, tol).into_iter().map(move |i| {
                let a = (r[i] - r[(i + n - 1) % n]).len();
                let b = (r[(i + 1) % n] - r[i]).len();
                ((k, i), r[i], a.min(b), a.max(b))
            })
        })
        .collect()
}

/// The map that takes `from` to `to` and turns the direction `base` into `goal` (after a
/// mirror across the x axis when `mirror`).
fn rigid_map(
    from: ddc_geom::V2,
    base: ddc_geom::V2,
    to: ddc_geom::V2,
    goal: ddc_geom::V2,
    mirror: bool,
) -> impl Fn(ddc_geom::V2) -> ddc_geom::V2 {
    let flip = move |p: ddc_geom::V2| if mirror { v2(p.x, -p.y) } else { p };
    let b = flip(base);
    let (s, c) = (goal.y.atan2(goal.x) - b.y.atan2(b.x)).sin_cos();
    move |p| {
        let d = flip(p - from);
        to + v2(c * d.x - s * d.y, s * d.x + c * d.y)
    }
}

/// Whether `new` is `old` moved as a whole: shifted, turned or mirrored, from any start
/// vertex in either direction, every ring in its place. A reshape is not.
fn moved_rigidly(new: &[Vec<ddc_geom::V2>], old: &[Vec<ddc_geom::V2>], tol: f64) -> bool {
    let same_counts =
        new.len() == old.len() && new.iter().zip(old).all(|(a, b)| a.len() == b.len());
    let (Some(a), Some(b)) = (old.first(), new.first()) else {
        return false;
    };
    let n = a.len();
    if !same_counts || n < 3 {
        return false;
    }
    let base = a[1] - a[0];
    for shift in 0..n {
        for rev in [false, true] {
            let at = |i: usize| if rev { b[(shift + n - i) % n] } else { b[(shift + i) % n] };
            let goal = at(1) - at(0);
            if (goal.len() - base.len()).abs() > tol {
                continue;
            }
            for mirror in [false, true] {
                let map = rigid_map(a[0], base, at(0), goal, mirror);
                let outer = (0..n).all(|i| (map(a[i]) - at(i)).len() <= tol);
                let holes = old[1..].iter().zip(&new[1..]).all(|(h, k)| {
                    h.iter().all(|p| k.iter().any(|q| (map(*p) - *q).len() <= tol))
                });
                if outer && holes {
                    return true;
                }
            }
        }
    }
    false
}

/// As `profile` for an edit of a saved outline `old`: a spike already saved there (its tip
/// unmoved, or the same spike carried by a move of the outline) never blocks the edit; a
/// spike that was not there, or one more than were saved, is refused as for new outlines.
pub(crate) fn profile_keeping(
    outer: &[(f64, f64)],
    holes: &[Vec<(f64, f64)>],
    old: &[Vec<(f64, f64)>],
) -> Result<Profile, Error> {
    let strict = profile(outer, holes);
    if strict.is_ok() {
        return strict;
    }
    let tol = Tol::default();
    let mut rs = rings_v2(&[outer.to_vec()]);
    rs.extend(rings_v2(holes));
    let old_v = rings_v2(old);
    let (tips, saved) = (spikes(&rs), spikes(&old_v));
    let near = |a: f64, b: f64| (a - b).abs() <= tol.length;
    // a moved spike of the same size is the saved one only when the whole outline moved, or
    // when the same vertex carries it (an edit that keeps the vertices, as walls drag them)
    let rigid = moved_rigidly(&rs, &old_v, tol.length);
    let same_vertices =
        rs.len() == old_v.len() && rs.iter().zip(&old_v).all(|(a, b)| a.len() == b.len());
    let legacy = |t: &Spike| {
        saved.iter().any(|s| {
            let carried = rigid || (same_vertices && s.0 == t.0);
            (s.1 - t.1).len() <= tol.length || (carried && near(s.2, t.2) && near(s.3, t.3))
        })
    };
    if tips.is_empty() || tips.len() > saved.len() || !tips.iter().all(legacy) {
        return strict;
    }
    let mut it = rs.into_iter();
    let o = it.next().unwrap_or_default();
    Profile::new_keeping_spikes(o, it.collect(), tol)
        .map_err(|e| Error::Geom(format!("slab boundary: {e:?}")))
}

/// Saved rings without their spikes; None when the outline has none.
pub(crate) fn cleaned(rs: &[Vec<(f64, f64)>]) -> Option<(Vec<Vec<(f64, f64)>>, usize)> {
    let vs = rings_v2(rs);
    let n = spikes(&vs).len();
    let tol = Tol::default();
    let back = |r: Vec<ddc_geom::V2>| r.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
    let out = vs
        .iter()
        .map(|r| back(ddc_geom::profile::remove_spikes(r, tol)))
        .collect();
    (n > 0).then_some((out, n))
}

/// A previewed clean-up of `what` `id` still applies: the saved rings `now` are the previewed
/// `before` and `after` is their clean-up. Otherwise refused, nothing written.
pub(crate) fn preview_holds(
    what: &str,
    id: i64,
    now: &[Vec<(f64, f64)>],
    before: &[Vec<(f64, f64)>],
    after: &[Vec<(f64, f64)>],
) -> Result<(), Error> {
    if now != before {
        return invalid(format!("{what} {id}: the outline changed since the preview"));
    }
    if after.is_empty() || cleaned(before).map(|(rs, _)| rs).as_deref() != Some(after) {
        return invalid(format!("{what} {id}: the clean-up is not the preview"));
    }
    Ok(())
}

pub(crate) fn check_slab_in(db: &Connection, s: &SlabSpec) -> Result<Profile, Error> {
    let lv = count(db, "SELECT count(*) FROM level WHERE id=?1", s.level)?;
    let ty = count(
        db,
        "SELECT count(*) FROM slab_type WHERE id=?1",
        s.slab_type,
    )?;
    if lv == 0 || ty == 0 || !s.offset.is_finite() {
        return invalid(format!(
            "slab: bad level {}, type {} or offset",
            s.level, s.slab_type
        ));
    }
    profile(&s.outer, &s.holes)
}

pub(crate) fn write_rings(db: &Connection, slab: i64, p: &Profile) -> Result<(), Error> {
    for (ring, pts) in p.rings().enumerate() {
        for (idx, q) in pts.iter().enumerate() {
            db.execute(
                "INSERT INTO slab_vertex VALUES (?1,?2,?3,?4,?5)",
                params![slab, ring as i64, idx as i64, q.x, q.y],
            )?;
        }
    }
    Ok(())
}

/// Slab outlines: [outer, holes...].
pub(crate) fn rings(db: &Connection, slab: i64) -> Result<Vec<Vec<(f64, f64)>>, Error> {
    let mut st =
        db.prepare_cached("SELECT ring, x, y FROM slab_vertex WHERE slab=?1 ORDER BY ring, idx")?;
    let rows = st.query_map([slab], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?)))?;
    let mut out: Vec<Vec<(f64, f64)>> = vec![];
    for row in rows {
        let (ring, x, y) = row?;
        if !(0..=1 << 16).contains(&ring) {
            return Err(Error::Geom(format!("slab {slab}: bad ring index {ring}")));
        }
        while out.len() <= ring as usize {
            out.push(vec![]);
        }
        out[ring as usize].push((x, y));
    }
    Ok(out)
}

impl Model {
    pub fn add_slab_type(&mut self, name: &str, layers: &[LayerSpec]) -> Result<i64, Error> {
        if layers.is_empty()
            || layers
                .iter()
                .any(|l| !(l.thickness.is_finite() && l.thickness > 0.0))
        {
            return invalid("slab type needs layers with positive thickness".into());
        }
        self.op("slab_type_add", |db| {
            db.execute("INSERT INTO slab_type(name) VALUES (?1)", [name])?;
            let id = db.last_insert_rowid();
            for (pos, l) in layers.iter().enumerate() {
                db.execute(
                    "INSERT INTO slab_type_layer VALUES (?1,?2,?3,?4)",
                    params![id, pos as i64, l.material, l.thickness],
                )?;
            }
            let inv = Inv::default()
                .delete("slab_type_layer", "stype", id)
                .delete("slab_type", "id", id);
            Ok((id, id, inv))
        })
    }

    pub fn delete_slab_type(&mut self, id: i64) -> Result<(), Error> {
        self.delete_ref(
            "slab_type",
            "slab_type_delete",
            id,
            &[("slab_type_layer", "stype")],
        )
    }

    fn check_slab(&self, s: &SlabSpec) -> Result<Profile, Error> {
        check_slab_in(&self.db, s)
    }

    pub fn add_slab(&mut self, s: &SlabSpec) -> Result<i64, Error> {
        Ok(self.add_slabs(std::slice::from_ref(s))?[0])
    }

    /// Several slabs in one operation (one undo).
    pub fn add_slabs(&mut self, ss: &[SlabSpec]) -> Result<Vec<i64>, Error> {
        if ss.is_empty() {
            return invalid("no slabs".into());
        }
        let profiles = ss
            .iter()
            .map(|s| self.check_slab(s))
            .collect::<Result<Vec<_>, _>>()?;
        self.op("slab_add", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            for (s, p) in ss.iter().zip(&profiles) {
                db.execute(
                    "INSERT INTO slab(level, stype, offset) VALUES (?1,?2,?3)",
                    params![s.level, s.slab_type, s.offset],
                )?;
                let id = db.last_insert_rowid();
                write_rings(db, id, p)?;
                inv = inv
                    .delete("slab_vertex", "slab", id)
                    .delete("slab", "id", id);
                ids.push(id);
            }
            Ok((ids.clone(), ids[0], inv))
        })
    }

    pub fn set_slab_boundary(
        &mut self,
        id: i64,
        outer: &[(f64, f64)],
        holes: &[Vec<(f64, f64)>],
    ) -> Result<(), Error> {
        let p = profile_keeping(outer, holes, &rings(&self.db, id)?)?;
        self.op("slab_boundary", |db| {
            if count(db, "SELECT count(*) FROM slab WHERE id=?1", id)? == 0 {
                return invalid(format!("no such slab {id}"));
            }
            let inv = Inv::default().delete("slab_vertex", "slab", id).restore(
                db,
                "slab_vertex",
                "slab",
                id,
            )?;
            db.execute("DELETE FROM slab_vertex WHERE slab=?1", [id])?;
            write_rings(db, id, &p)?;
            Ok(((), id, inv))
        })
    }

    /// Preview of «Clean up outline»: the rings without spikes and the number of spikes;
    /// None when the outline has none. Reads only.
    pub fn slab_outline_cleanup(
        &self,
        id: i64,
    ) -> Result<Option<(Vec<Vec<(f64, f64)>>, usize)>, Error> {
        Ok(cleaned(&rings(&self.db, id)?))
    }

    /// Drops the spikes of a saved outline as one undo step; the count of spikes removed.
    /// Runs only on request, never on open.
    pub fn clean_slab_outline(&mut self, id: i64) -> Result<usize, Error> {
        let before = rings(&self.db, id)?;
        let Some((rs, n)) = cleaned(&before) else {
            return Ok(0);
        };
        self.apply_slab_outline_cleanup(id, &before, &rs).map(|()| n)
    }

    /// Writes exactly the previewed `after` rings as one undo step. Refused when the saved
    /// outline is no longer `before` (changed since the preview) or `after` is not its clean-up.
    pub fn apply_slab_outline_cleanup(
        &mut self,
        id: i64,
        before: &[Vec<(f64, f64)>],
        after: &[Vec<(f64, f64)>],
    ) -> Result<(), Error> {
        preview_holds("slab", id, &rings(&self.db, id)?, before, after)?;
        let p = profile(&after[0], &after[1..])?;
        self.op("slab_boundary", |db| {
            let inv = Inv::default().delete("slab_vertex", "slab", id).restore(
                db,
                "slab_vertex",
                "slab",
                id,
            )?;
            db.execute("DELETE FROM slab_vertex WHERE slab=?1", [id])?;
            write_rings(db, id, &p)?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_slab(&mut self, id: i64) -> Result<(), Error> {
        self.op("slab_delete", |db| {
            let inv = Inv::default().restore(db, "slab", "id", id)?.restore(
                db,
                "slab_vertex",
                "slab",
                id,
            )?;
            if db.execute("DELETE FROM slab WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such slab {id}"));
            }
            db.execute("DELETE FROM slab_vertex WHERE slab=?1", [id])?;
            let inv = crate::components::drop_hosted(db, inv, "host_slab", id)?;
            let inv = crate::sketch::drop_owned(db, inv, "slab", id)?;
            let inv = crate::roof_edge::drop_hosts(db, inv, "slab", &[id])?;
            Ok(((), id, inv))
        })
    }

    /// Quantities per layer from top to bottom.
    pub fn slab_quantities(&self, id: i64) -> Result<Vec<SlabQty>, Error> {
        let mut st = self.db.prepare(
            "SELECT q.material, l.thickness, q.area, q.volume FROM slab_qty q \
             JOIN slab s ON s.id=q.slab JOIN slab_type_layer l ON l.stype=s.stype AND l.pos=q.pos \
             WHERE q.slab=?1 ORDER BY q.pos",
        )?;
        let f = self
            .slab_slope(id)
            .ok()
            .flatten()
            .map_or(1.0, |p| p.area_factor());
        let rows = st.query_map([id], |r| {
            let area: f64 = r.get(2)?;
            Ok(SlabQty {
                material: r.get(0)?,
                thickness: r.get(1)?,
                area,
                surface_area: area * f,
                volume: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Slab outlines: [outer (counter-clockwise), holes (clockwise)...].
    pub fn slab_rings(&self, id: i64) -> Result<Vec<Vec<(f64, f64)>>, Error> {
        rings(&self.db, id)
    }

    /// Top slope: `offset` is the top elevation at point `p0`; None or zero slope means flat.
    pub fn set_slab_slope(&mut self, id: i64, slope: Option<Plane>) -> Result<(), Error> {
        let text = match slope.filter(|p| !p.is_flat()) {
            Some(p) if [p.p0.x, p.p0.y, p.gx, p.gy].iter().all(|v| v.is_finite()) => {
                if p.gx.hypot(p.gy) > 10.0 {
                    return invalid("slab slope steeper than 10:1".into());
                }
                Some(slope_text(&p))
            }
            Some(_) => return invalid("slab slope: non-finite".into()),
            None => None,
        };
        self.op("slab_slope", |db| {
            if count(db, "SELECT count(*) FROM slab WHERE id=?1", id)? == 0 {
                return invalid(format!("no such slab {id}"));
            }
            let inv = Inv::default()
                .delete("slab", "id", id)
                .restore(db, "slab", "id", id)?;
            db.execute("UPDATE slab SET slope=?1 WHERE id=?2", params![text, id])?;
            Ok(((), id, inv))
        })
    }

    pub fn slab_slope(&self, id: i64) -> Result<Option<Plane>, Error> {
        let t: Option<String> =
            self.db
                .query_row("SELECT slope FROM slab WHERE id=?1", [id], |r| r.get(0))?;
        t.map(|t| parse_slope(&t)).transpose()
    }

    /// Slab solid (top follows the slope, type thickness is vertical).
    pub fn slab_mesh(&self, id: i64) -> Result<ddc_geom::mesh::Mesh, Error> {
        let t: f64 = self.db.query_row(
            "SELECT (SELECT SUM(thickness) FROM slab_type_layer WHERE stype=s.stype)              FROM slab s WHERE s.id=?1",
            [id],
            |r| r.get(0),
        )?;
        slab_meshes(&self.db, id, &[t]).map(|mut v| v.remove(0))
    }

    /// Slab solid with extra through-holes (not crossing the outlines).
    /// With holes the solid is built from the slab outlines: shaft cutouts and the sketch
    /// shape are not taken into account in that case.
    pub(crate) fn slab_mesh_holes(
        &self,
        id: i64,
        extra: &[Vec<(f64, f64)>],
    ) -> Result<ddc_geom::mesh::Mesh, Error> {
        if extra.is_empty() {
            return self.slab_mesh(id);
        }
        let (top, t): (f64, f64) = self.db.query_row(
            "SELECT lv.elevation + s.offset, (SELECT SUM(thickness) FROM slab_type_layer              WHERE stype=s.stype) FROM slab s JOIN level lv ON lv.id=s.level WHERE s.id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let rs = rings(&self.db, id)?;
        let (outer, holes) = rs
            .split_first()
            .ok_or_else(|| Error::Geom(format!("slab {id}: no boundary")))?;
        let all: Vec<Vec<(f64, f64)>> = holes.iter().chain(extra).cloned().collect();
        let p = profile_keeping(outer, &all, &rs)?;
        let plane = self.slab_slope(id)?.unwrap_or(Plane {
            p0: v2(outer[0].0, outer[0].1),
            gx: 0.0,
            gy: 0.0,
        });
        ddc_geom::slope::slab_mesh(&p, top, &plane, t, Tol::default())
            .map_err(|e| Error::Geom(format!("slab {id}: {e:?}")))
    }
}

/// Slab layers from top to bottom: (layer position in the type, layer solid following the slope, with holes).
pub(crate) fn layer_meshes(
    db: &Connection,
    id: i64,
) -> Result<Vec<(i64, ddc_geom::mesh::Mesh)>, Error> {
    let layers: Vec<(i64, f64)> = db
        .prepare(
            "SELECT l.pos,l.thickness FROM slab_type_layer l JOIN slab s ON s.stype=l.stype              WHERE s.id=?1 ORDER BY l.pos",
        )?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let th: Vec<f64> = layers.iter().map(|l| l.1).collect();
    let meshes = slab_meshes(db, id, &th)?;
    Ok(layers.into_iter().map(|l| l.0).zip(meshes).collect())
}

/// Layer solids of thicknesses `th` from top to bottom (the top slope is shared). For a sketch
/// shape, its own solid; otherwise the plan after cutouts (shafts + openings, edge notches,
/// pieces), the same as in the quantities.
fn slab_meshes(db: &Connection, id: i64, th: &[f64]) -> Result<Vec<ddc_geom::mesh::Mesh>, Error> {
    let (top, slope): (f64, Option<String>) = db.query_row(
        "SELECT lv.elevation + s.offset, s.slope FROM slab s JOIN level lv ON lv.id=s.level          WHERE s.id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let rs = rings(db, id)?;
    let outer = rs
        .first()
        .ok_or_else(|| Error::Geom(format!("slab {id}: no boundary")))?;
    let slope = slope.map(|t| parse_slope(&t)).transpose()?;
    let plane = slope.unwrap_or(Plane {
        p0: v2(outer[0].0, outer[0].1),
        gx: 0.0,
        gy: 0.0,
    });
    let shape = crate::sketch::shape_mesh(db, id)?;
    let cut = match shape {
        Some(_) => None,
        None => Some(
            crate::cuts::slab_cut(db, id)?
                .ok_or_else(|| Error::Geom(format!("slab {id}: no boundary")))?,
        ),
    };
    let mut z = top;
    let mut out = vec![];
    for &t in th {
        if let Some(m) = &shape {
            let mut mesh = m.mesh(z, t);
            if let Some(pl) = &slope {
                for p in mesh.positions.iter_mut() {
                    p.z += pl.rise(v2(p.x, p.y));
                }
            }
            out.push(mesh);
        } else if let Some(cut) = &cut {
            let mut ms = vec![];
            for (o, hs) in &cut.pieces {
                // a spike saved before they were refused still draws
                let p = Profile::new_keeping_spikes(o.clone(), hs.clone(), Tol::default())
                    .map_err(|e| Error::Geom(format!("slab {id}: {e:?}")))?;
                ms.push(
                    ddc_geom::slope::slab_mesh(&p, z, &plane, t, Tol::default())
                        .map_err(|e| Error::Geom(format!("slab {id}: {e:?}")))?,
                );
            }
            out.push(crate::cuts::concat(ms));
        }
        z -= t;
    }
    Ok(out)
}

/// `x y gx gy`.
pub fn slope_text(p: &Plane) -> String {
    format!("{} {} {} {}", p.p0.x, p.p0.y, p.gx, p.gy)
}

pub fn parse_slope(t: &str) -> Result<Plane, Error> {
    let v: Vec<f64> = t
        .split_whitespace()
        .map(|x| x.parse::<f64>().ok().filter(|v| v.is_finite()))
        .collect::<Option<_>>()
        .ok_or_else(|| Error::Invalid(format!("bad slab slope '{t}'")))?;
    match v.as_slice() {
        [x, y, gx, gy] => Ok(Plane {
            p0: v2(*x, *y),
            gx: *gx,
            gy: *gy,
        }),
        _ => Err(Error::Invalid(format!("bad slab slope '{t}'"))),
    }
}

/// Quantities of one slab (layer: pos, material, area, volume).
fn slab_rows(db: &Connection, id: i64, stype: i64) -> Result<Vec<(i64, String, f64, f64)>, Error> {
    let rs = rings(db, id)?;
    let Some((outer, holes)) = rs.split_first() else {
        return Err(Error::Geom(format!("slab {id}: no boundary")));
    };
    let area = saved_profile(outer, holes)
        .map_err(|e| Error::Geom(format!("slab {id}: {e}")))?
        .area();
    let mut st = db.prepare(
        "SELECT pos, material, thickness FROM slab_type_layer WHERE stype=?1 ORDER BY pos",
    )?;
    let layers: Vec<(i64, String, f64)> = st
        .query_map([stype], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let t: f64 = layers.iter().map(|l| l.2).sum();
    // plan after cutouts: shafts and openings merged (no double deduction), edge notches
    let area = match crate::cuts::slab_cut(db, id)? {
        Some(c) => c.area(),
        None => area,
    };
    // a shape with variable thickness: the volume difference goes to the top layer (the screed)
    let extra = match crate::sketch::shape_mesh(db, id) {
        Ok(Some(m)) => m.volume(t) - m.plan_area() * t,
        _ => 0.0,
    };
    Ok(layers
        .into_iter()
        .enumerate()
        .map(|(k, (p, m, t))| (p, m, area, area * t + if k == 0 { extra } else { 0.0 }))
        .collect())
}

/// Slabs with an invalid outline: (id, reason). They are not recomputed and do not block edits.
pub(crate) fn invalid_slabs(db: &Connection) -> Result<Vec<(i64, String)>, Error> {
    let slabs: Vec<(i64, i64)> = {
        let mut st = db.prepare("SELECT id, stype FROM slab ORDER BY id")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut out = vec![];
    // outline only: layers and shafts do not affect validity
    for (id, _) in slabs {
        let rs = match rings(db, id) {
            Err(Error::Geom(m)) => {
                out.push((id, m));
                continue;
            }
            r => r?,
        };
        let why = match rs.split_first() {
            None => Some("no boundary".to_string()),
            Some((outer, holes)) => saved_profile(outer, holes).err().map(|e| e.to_string()),
        };
        if let Some(w) = why {
            out.push((id, format!("slab {id}: {w}")));
        }
    }
    Ok(out)
}

/// Recomputes `slab_qty`: profile area with holes × layer thickness (`only` limits it to these
/// slabs). An invalid slab is skipped (its previous quantities remain), see `invalid_slabs`.
/// Returns the rewritten slabs (columns are subtracted from them).
pub(crate) fn regenerate(
    db: &Connection,
    only: Option<&std::collections::BTreeSet<i64>>,
) -> Result<std::collections::HashSet<i64>, Error> {
    let slabs: Vec<(i64, i64)> = match only {
        None => {
            db.execute(
                "DELETE FROM slab_qty WHERE slab NOT IN (SELECT id FROM slab)",
                [],
            )?;
            let mut st = db.prepare("SELECT id, stype FROM slab ORDER BY id")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        }
        Some(ids) => {
            let mut st = db.prepare_cached("SELECT stype FROM slab WHERE id=?1")?;
            let mut v = vec![];
            for &id in ids {
                match st.query_row([id], |r| r.get::<_, i64>(0)) {
                    Ok(t) => v.push((id, t)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        db.execute("DELETE FROM slab_qty WHERE slab=?1", [id])?;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            v
        }
    };
    let mut done = std::collections::HashSet::new();
    let mut ins = db.prepare_cached("INSERT INTO slab_qty VALUES (?1,?2,?3,?4,?5)")?;
    for (id, stype) in slabs {
        let rows = match slab_rows(db, id, stype) {
            Ok(r) => r,
            Err(Error::Geom(_)) => continue,
            Err(e) => return Err(e),
        };
        db.execute("DELETE FROM slab_qty WHERE slab=?1", [id])?;
        for (pos, m, area, vol) in rows {
            ins.execute(params![id, pos, m, area, vol])?;
        }
        done.insert(id);
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concrete(m: &mut Model) -> (i64, i64) {
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_slab_type(
                "floor",
                &[LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        (lv, t)
    }

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    }

    #[test]
    fn slab_6x4_is_4_8_m3_and_hole_deducts() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, t) = concrete(&mut m);
        let s = m
            .add_slab(&SlabSpec {
                level: lv,
                slab_type: t,
                offset: 0.0,
                outer: rect(0., 0., 6., 4.),
                holes: vec![],
            })
            .unwrap();
        m.assert_integrity();
        let q = m.slab_quantities(s).unwrap();
        assert_eq!(q.len(), 1);
        assert!((q[0].area - 24.0).abs() < 1e-12 && (q[0].volume - 4.8).abs() < 1e-12);
        m.set_slab_boundary(s, &rect(0., 0., 6., 4.), &[rect(1., 1., 2., 2.)])
            .unwrap();
        let q = m.slab_quantities(s).unwrap();
        assert!((q[0].area - 23.0).abs() < 1e-12 && (q[0].volume - 4.6).abs() < 1e-12);
        // A hole is stored clockwise.
        let r = m.slab_rings(s).unwrap();
        assert_eq!(r.len(), 2);
        assert!(
            ddc_geom::profile::signed_area(&r[1].iter().map(|p| v2(p.0, p.1)).collect::<Vec<_>>())
                < 0.0
        );
        m.undo().unwrap();
        assert!((m.slab_quantities(s).unwrap()[0].volume - 4.8).abs() < 1e-12);
        m.assert_integrity();
    }

    #[test]
    fn sloped_slab_keeps_exact_volume_and_undoes() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, t) = concrete(&mut m);
        let s = m
            .add_slab(&SlabSpec {
                level: lv,
                slab_type: t,
                offset: 3.0,
                outer: rect(0., 0., 6., 4.),
                holes: vec![],
            })
            .unwrap();
        let pl = Plane::arrow(v2(0., 0.), v2(6., 0.), 0.12).unwrap();
        m.set_slab_slope(s, Some(pl)).unwrap();
        assert_eq!(m.slab_slope(s).unwrap(), Some(pl));
        let mesh = m.slab_mesh(s).unwrap();
        assert!(mesh.is_watertight() && (mesh.volume() - 4.8).abs() < 1e-12);
        let q = &m.slab_quantities(s).unwrap()[0];
        assert!((q.volume - 4.8).abs() < 1e-12 && (q.area - 24.0).abs() < 1e-12);
        assert!((q.surface_area - 24.0 * (1.0f64 + 0.02 * 0.02).sqrt()).abs() < 1e-12);
        let zmax = mesh.positions.iter().map(|q| q.z).fold(f64::MIN, f64::max);
        assert!((zmax - 3.12).abs() < 1e-12);
        assert!(m
            .set_slab_slope(
                s,
                Some(Plane {
                    p0: v2(0., 0.),
                    gx: 20.0,
                    gy: 0.0
                })
            )
            .is_err());
        m.undo().unwrap();
        assert_eq!(m.slab_slope(s).unwrap(), None);
        m.assert_integrity();
    }

    #[test]
    fn bad_slab_ring_does_not_block_other_edits() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, t) = concrete(&mut m);
        let spec = |outer| SlabSpec {
            level: lv,
            slab_type: t,
            offset: 0.0,
            outer,
            holes: vec![],
        };
        let a = m.add_slab(&spec(rect(0., 0., 6., 4.))).unwrap();
        let b = m.add_slab(&spec(rect(10., 0., 12., 2.))).unwrap();
        // Corrupt slab a into a bow-tie behind the model's back.
        m.db.execute(
            "UPDATE slab_vertex SET x=CASE idx WHEN 2 THEN 0.0 ELSE 6.0 END WHERE slab=?1 AND idx>=2",
            [a],
        )
        .unwrap();
        m.set_slab_boundary(b, &rect(10., 0., 12., 3.), &[])
            .unwrap();
        assert!((m.slab_quantities(b).unwrap()[0].area - 6.0).abs() < 1e-12);
        // Last good quantities of the bad slab are kept and reported.
        assert!((m.slab_quantities(a).unwrap()[0].area - 24.0).abs() < 1e-12);
        let v = m.check_integrity().unwrap();
        assert!(v.iter().any(|i| i.table == "slab" && i.id == a), "{v:?}");
        m.undo().unwrap();
        assert!((m.slab_quantities(b).unwrap()[0].area - 4.0).abs() < 1e-12);
    }

    #[test]
    fn bad_boundary_and_refs_rejected() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, t) = concrete(&mut m);
        let bow = vec![(0., 0.), (1., 1.), (1., 0.), (0., 1.)];
        let spec = |outer, level, slab_type| SlabSpec {
            level,
            slab_type,
            offset: 0.0,
            outer,
            holes: vec![],
        };
        assert!(matches!(m.add_slab(&spec(bow, lv, t)), Err(Error::Geom(_))));
        assert!(m.add_slab(&spec(rect(0., 0., 1., 1.), 99, t)).is_err());
        assert!(m.add_slab(&spec(rect(0., 0., 1., 1.), lv, 99)).is_err());
        let s = m.add_slab(&spec(rect(0., 0., 1., 1.), lv, t)).unwrap();
        // The type and level are in use by a slab.
        assert!(matches!(
            m.delete_slab_type(t),
            Err(Error::Referenced { .. })
        ));
        assert!(matches!(m.delete_level(lv), Err(Error::Referenced { .. })));
        m.delete_slab(s).unwrap();
        assert_eq!(
            count(&m.db, "SELECT count(*) FROM slab_vertex WHERE slab=?1", s).unwrap(),
            0
        );
        m.delete_slab_type(t).unwrap();
        m.assert_integrity();
        m.undo().unwrap();
        m.undo().unwrap();
        m.assert_integrity();
        assert_eq!(m.slab_quantities(s).unwrap().len(), 1);
    }

    /// The 45-degree snap spike of creation fuzz seed 3 (tip at index 1), CCW.
    fn spiked() -> Vec<(f64, f64)> {
        let s = std::f64::consts::FRAC_1_SQRT_2;
        let (cx, cy) = (4.0 - s * 1.5, 4.0 - s * 1.5);
        vec![(1., 1.), (4., 4.), (cx, cy + 1e-15), (0., 3.)]
    }

    /// A floor saved before spikes were refused: its rings are written past the check.
    fn legacy_floor(m: &mut Model) -> i64 {
        let (lv, t) = concrete(m);
        let spec = SlabSpec {
            level: lv,
            slab_type: t,
            offset: 0.0,
            outer: rect(0., 0., 4., 4.),
            holes: vec![],
        };
        let s = m.add_slab(&spec).unwrap();
        m.db.execute("DELETE FROM slab_vertex WHERE slab=?1", [s]).unwrap();
        for (i, p) in spiked().iter().enumerate() {
            m.db.execute(
                "INSERT INTO slab_vertex VALUES (?1,0,?2,?3,?4)",
                params![s, i as i64, p.0, p.1],
            )
            .unwrap();
        }
        assert!(profile(&spiked(), &[]).is_err(), "the fixture must be a spike");
        s
    }

    #[test]
    fn legacy_spike_never_blocks_an_edit() {
        let mut m = Model::open(":memory:").unwrap();
        let s = legacy_floor(&mut m);
        assert!(m.slab_mesh(s).is_ok(), "a saved spike still draws");
        // moving another vertex keeps the old spike: allowed
        let mut moved = spiked();
        moved[3] = (0., 3.5);
        m.set_slab_boundary(s, &moved, &[]).unwrap();
        assert_eq!(m.slab_rings(s).unwrap()[0], moved);
        // adding a hole elsewhere is an edit too
        let hole = vec![(1.0, 2.0), (1.2, 2.0), (1.2, 2.2), (1.0, 2.2)];
        m.set_slab_boundary(s, &moved, &[hole]).unwrap();
        m.assert_integrity();
    }

    #[test]
    fn new_spike_is_still_refused() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, t) = concrete(&mut m);
        let spec = |outer| SlabSpec {
            level: lv,
            slab_type: t,
            offset: 0.0,
            outer,
            holes: vec![],
        };
        assert!(matches!(m.add_slab(&spec(spiked())), Err(Error::Geom(_))));
        let s = m.add_slab(&spec(rect(0., 0., 4., 4.))).unwrap();
        let r = m.set_slab_boundary(s, &spiked(), &[]);
        assert!(matches!(r, Err(Error::Geom(_))), "{r:?}");
        assert_eq!(m.slab_rings(s).unwrap()[0], rect(0., 0., 4., 4.));
        // a legacy floor: a spike whose tip was not saved is new, so it is refused
        let mut m = Model::open(":memory:").unwrap();
        let l = legacy_floor(&mut m);
        let mut tip_moved = spiked();
        tip_moved[1] = (5., 5.);
        let r = m.set_slab_boundary(l, &tip_moved, &[]);
        assert!(matches!(r, Err(Error::Geom(_))), "{r:?}");
        assert_eq!(m.slab_rings(l).unwrap()[0], spiked());
    }

    #[test]
    fn clean_up_outline_is_valid_and_one_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let s = legacy_floor(&mut m);
        let (preview, n) = m.slab_outline_cleanup(s).unwrap().expect("a spike to clean");
        assert_eq!(n, 1);
        assert_eq!(m.slab_rings(s).unwrap()[0], spiked(), "the preview writes nothing");
        assert_eq!(m.clean_slab_outline(s).unwrap(), 1);
        let after = m.slab_rings(s).unwrap();
        assert_eq!(after, preview);
        assert_eq!(after[0].len(), 3);
        assert!(profile(&after[0], &after[1..]).is_ok(), "the clean outline is valid");
        assert!(m.slab_outline_cleanup(s).unwrap().is_none());
        assert_eq!(m.clean_slab_outline(s).unwrap(), 0, "nothing left to clean");
        m.assert_integrity();
        m.undo().unwrap();
        assert_eq!(m.slab_rings(s).unwrap()[0], spiked(), "one undo brings the spike back");
    }

    #[test]
    fn clean_up_writes_the_preview_or_refuses_a_changed_outline() {
        let mut m = Model::open(":memory:").unwrap();
        let s = legacy_floor(&mut m);
        let before = m.slab_rings(s).unwrap();
        let (after, _) = m.slab_outline_cleanup(s).unwrap().unwrap();
        // the floor moves under the preview: a vertex away from the spike
        let mut moved = spiked();
        moved[3].1 += 0.5;
        m.set_slab_boundary(s, &moved, &[]).unwrap();
        let r = m.apply_slab_outline_cleanup(s, &before, &after);
        assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
        assert_eq!(m.slab_rings(s).unwrap()[0], moved, "a refused apply writes nothing");
        // a preview that is not the clean-up of `before` is refused too
        let now = m.slab_rings(s).unwrap();
        let r = m.apply_slab_outline_cleanup(s, &now, &after);
        assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
        let (fresh, _) = m.slab_outline_cleanup(s).unwrap().unwrap();
        m.apply_slab_outline_cleanup(s, &now, &fresh).unwrap();
        assert_eq!(m.slab_rings(s).unwrap(), fresh, "exactly the preview is written");
    }
}
