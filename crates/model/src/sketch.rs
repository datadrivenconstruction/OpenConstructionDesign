// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Outline sketches, slab shape, wall profile on the elevation, and wall bottom/top attachment.
//! Tables (schema 17, additive, created by name with `CREATE TABLE IF NOT EXISTS`):
//! `sketch` holds the host's sketch curves (slab|roof|ceiling|shaft|opening), `slab_shape` the
//! shape points and lines, `wall_profile` the elevation polygon, `wall_attach` the bottom/top
//! attachment to a slab. Attachments store no derived heights: they are read from the host's
//! current shape (and follow it).

use ddc_geom::math::{v2, V2};
use ddc_geom::shape_edit::{ShapeLine, ShapeMesh, SlabShape, Thickness};
use ddc_geom::sketch::{ElevationProfile, Sketch, SketchCurve};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS sketch(host_kind TEXT NOT NULL, host INTEGER NOT NULL, data TEXT NOT NULL,
    PRIMARY KEY(host_kind, host));
CREATE TABLE IF NOT EXISTS slab_shape(slab INTEGER PRIMARY KEY, mode INTEGER NOT NULL, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS wall_profile(wall INTEGER PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS wall_attach(wall INTEGER NOT NULL, side TEXT NOT NULL, slab INTEGER NOT NULL,
    PRIMARY KEY(wall, side));
";

/// Tables of this module, idempotent, keyed by table existence.
pub(crate) fn ensure(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    db.execute_batch(crate::cuts::DDL)?;
    crate::cuts::migrate_sketch_kind(db)?;
    Ok(())
}

/// A host's sketches, shape, profile and attachments are deleted with it; undo reinserts them.
/// `kind` is the host key (`SketchHost::key`): slab|roof|ceiling|shaft|opening|host_opening|wall.
pub(crate) fn drop_owned(db: &Connection, mut inv: Inv, kind: &str, id: i64) -> Result<Inv, Error> {
    {
        let mut st = db.prepare("SELECT data FROM sketch WHERE host_kind=?1 AND host=?2")?;
        let rows: Vec<String> = st
            .query_map(params![kind, id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for d in rows {
            inv = inv.sql(
                "INSERT OR REPLACE INTO sketch(host_kind, host, data) VALUES (?1, ?2, ?3)",
                vec![kind.to_string().into(), id.into(), d.into()],
            );
        }
    }
    db.execute(
        "DELETE FROM sketch WHERE host_kind=?1 AND host=?2",
        params![kind, id],
    )?;
    let owned: &[(&str, &str)] = match kind {
        "slab" => &[("slab_shape", "slab"), ("wall_attach", "slab")],
        "wall" => &[
            ("wall_profile", "wall"),
            ("wall_attach", "wall"),
            ("wall_attach_level", "wall"),
        ],
        _ => &[],
    };
    for &(t, c) in owned {
        inv = inv.restore(db, t, c, id)?;
        db.execute(&format!("DELETE FROM {t} WHERE {c}=?1"), [id])?;
    }
    if matches!(kind, "slab" | "roof" | "ceiling") {
        inv = crate::cuts::drop_openings(db, inv, kind, id)?;
    }
    Ok(inv)
}

/// Sketches without a host (a polymorphic reference, outside the common reference table).
pub(crate) fn orphan_sketches(db: &Connection) -> Result<Vec<crate::integrity::Issue>, Error> {
    let mut st = db.prepare("SELECT rowid, host_kind, host FROM sketch ORDER BY rowid")?;
    let rows: Vec<(i64, String, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let mut out = vec![];
    for (rid, k, h) in rows {
        let table = match k.as_str() {
            "slab" | "roof" | "ceiling" | "shaft" | "opening" | "host_opening" | "wall" => {
                k.as_str()
            }
            _ => {
                out.push(crate::integrity::Issue {
                    table: "sketch".into(),
                    id: rid,
                    problem: format!("unknown host kind {k}"),
                });
                continue;
            }
        };
        if !exists(db, table, h)? {
            out.push(crate::integrity::Issue {
                table: "sketch".into(),
                id: rid,
                problem: format!("host -> missing {k} {h}"),
            });
        }
    }
    out.extend(crate::cuts::orphans(db)?);
    Ok(out)
}

/// Sketch host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SketchHost {
    Slab(i64),
    Roof(i64),
    Ceiling(i64),
    Shaft(i64),
    /// Wall opening (door/window, table `opening`).
    Opening(i64),
    /// Sketch opening in a slab/roof/ceiling (table `host_opening`).
    HostOpening(i64),
}

impl SketchHost {
    pub(crate) fn key(self) -> (&'static str, i64) {
        match self {
            SketchHost::Slab(i) => ("slab", i),
            SketchHost::Roof(i) => ("roof", i),
            SketchHost::Ceiling(i) => ("ceiling", i),
            SketchHost::Shaft(i) => ("shaft", i),
            SketchHost::Opening(i) => ("opening", i),
            SketchHost::HostOpening(i) => ("host_opening", i),
        }
    }
}

/// Wall side for attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallSide {
    Base,
    Top,
}

impl WallSide {
    fn key(self) -> &'static str {
        match self {
            WallSide::Base => "base",
            WallSide::Top => "top",
        }
    }
}

/// Sagitta used to subdivide arcs when writing an outline into kernel rings.
pub const SKETCH_SAG: f64 = 0.002;

fn invalid<T>(m: String) -> Result<T, Error> {
    Err(Error::Invalid(m))
}

fn nums(s: &str) -> Result<Vec<f64>, Error> {
    s.split_whitespace()
        .map(|t| {
            t.parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| Error::Invalid(format!("bad number {t}")))
        })
        .collect()
}

/// First character (any width) and the rest; a multibyte tag must not split inside it.
fn split_tag(line: &str) -> (&str, &str) {
    line.split_at(line.chars().next().map_or(0, char::len_utf8))
}

/// Largest ring and vertex index a shape row may name (a row is untrusted text).
const MAX_SHAPE_INDEX: usize = 100_000;

/// One row per curve: `L ax ay bx by` | `A cx cy r a0 sweep` | `E cx cy rx ry rot t0 sweep` | `S x y …`.
pub fn sketch_text(sk: &Sketch) -> String {
    let f = |v: &[f64]| {
        v.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    };
    sk.curves
        .iter()
        .map(|c| match c {
            SketchCurve::Line { a, b } => format!("L {}", f(&[a.x, a.y, b.x, b.y])),
            SketchCurve::Arc { c, r, a0, sweep } => {
                format!("A {}", f(&[c.x, c.y, *r, *a0, *sweep]))
            }
            SketchCurve::Ellipse {
                c,
                rx,
                ry,
                rot,
                t0,
                sweep,
            } => {
                format!("E {}", f(&[c.x, c.y, *rx, *ry, *rot, *t0, *sweep]))
            }
            SketchCurve::Spline { ctrl } => {
                let v: Vec<f64> = ctrl.iter().flat_map(|p| [p.x, p.y]).collect();
                format!("S {}", f(&v))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn parse_sketch(t: &str) -> Result<Sketch, Error> {
    let mut curves = vec![];
    for line in t.lines().filter(|l| !l.trim().is_empty()) {
        let (k, rest) = split_tag(line);
        let v = nums(rest)?;
        let c = match (k, v.len()) {
            ("L", 4) => SketchCurve::Line {
                a: v2(v[0], v[1]),
                b: v2(v[2], v[3]),
            },
            ("A", 5) => SketchCurve::Arc {
                c: v2(v[0], v[1]),
                r: v[2],
                a0: v[3],
                sweep: v[4],
            },
            ("E", 7) => SketchCurve::Ellipse {
                c: v2(v[0], v[1]),
                rx: v[2],
                ry: v[3],
                rot: v[4],
                t0: v[5],
                sweep: v[6],
            },
            ("S", n) if n >= 4 && n % 2 == 0 => SketchCurve::Spline {
                ctrl: v.chunks(2).map(|p| v2(p[0], p[1])).collect(),
            },
            _ => return invalid(format!("bad sketch curve: {line}")),
        };
        curves.push(c);
    }
    Ok(Sketch { curves })
}

/// `R ring vertex dz` | `P x y dz` | `S ax ay bx by dza dzb`.
pub fn shape_text(s: &SlabShape) -> String {
    let mut out = vec![];
    for (r, ring) in s.ring_dz.iter().enumerate() {
        for (k, z) in ring.iter().enumerate().filter(|(_, z)| **z != 0.0) {
            out.push(format!("R {r} {k} {z}"));
        }
    }
    for (p, z) in &s.points {
        out.push(format!("P {} {} {z}", p.x, p.y));
    }
    for l in &s.lines {
        out.push(format!(
            "S {} {} {} {} {} {}",
            l.a.x, l.a.y, l.b.x, l.b.y, l.dza, l.dzb
        ));
    }
    out.join("\n")
}

pub fn parse_shape(t: &str, mode: Thickness) -> Result<SlabShape, Error> {
    let mut s = SlabShape {
        mode,
        ..Default::default()
    };
    for line in t.lines().filter(|l| !l.trim().is_empty()) {
        let (k, rest) = split_tag(line);
        let v = nums(rest)?;
        match (k, v.len()) {
            ("R", 3) => {
                let (r, i) = (v[0] as usize, v[1] as usize);
                if r > MAX_SHAPE_INDEX || i > MAX_SHAPE_INDEX {
                    return invalid(format!("shape row index too large: {line}"));
                }
                if s.ring_dz.len() <= r {
                    s.ring_dz.resize(r + 1, vec![]);
                }
                if s.ring_dz[r].len() <= i {
                    s.ring_dz[r].resize(i + 1, 0.0);
                }
                s.ring_dz[r][i] = v[2];
            }
            ("P", 3) => s.points.push((v2(v[0], v[1]), v[2])),
            ("S", 6) => s.lines.push(ShapeLine {
                a: v2(v[0], v[1]),
                b: v2(v[2], v[3]),
                dza: v[4],
                dzb: v[5],
            }),
            _ => return invalid(format!("bad shape row: {line}")),
        }
    }
    Ok(s)
}

fn profile_text(p: &ElevationProfile) -> String {
    std::iter::once(&p.outer)
        .chain(&p.holes)
        .map(|r| {
            r.iter()
                .map(|(u, z)| format!("{u} {z}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn parse_profile(t: &str) -> Result<ElevationProfile, Error> {
    let mut rings = vec![];
    for line in t.lines().filter(|l| !l.trim().is_empty()) {
        let v = nums(line)?;
        rings.push(
            v.chunks(2)
                .filter(|c| c.len() == 2)
                .map(|c| (c[0], c[1]))
                .collect::<Vec<_>>(),
        );
    }
    let mut it = rings.into_iter();
    let outer = it
        .next()
        .ok_or_else(|| Error::Invalid("empty wall profile".into()))?;
    Ok(ElevationProfile {
        outer,
        holes: it.collect(),
    })
}

fn slab_rings(db: &Connection, id: i64) -> Result<Vec<Vec<V2>>, Error> {
    Ok(crate::slabs::rings(db, id)?
        .into_iter()
        .map(|r| r.into_iter().map(|(x, y)| v2(x, y)).collect())
        .collect())
}

fn exists(db: &Connection, table: &str, id: i64) -> Result<bool, Error> {
    let n: i64 = db.query_row(
        &format!("SELECT count(*) FROM {table} WHERE id=?1"),
        [id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Slab top elevation (level + offset) and total thickness.
pub(crate) fn slab_top_t(db: &Connection, id: i64) -> Result<(f64, f64), Error> {
    db.query_row(
        "SELECT lv.elevation + s.offset, (SELECT COALESCE(SUM(thickness),0) FROM slab_type_layer \
         WHERE stype=s.stype) FROM slab s JOIN level lv ON lv.id=s.level WHERE s.id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("no such slab {id}")))
}

pub(crate) fn shape_mesh(db: &Connection, id: i64) -> Result<Option<ShapeMesh>, Error> {
    Ok(shape_mesh_ex(db, id)?.map(|m| m.0))
}

/// Slab shape over the plan after cutouts; flag false means it could not be built with the
/// cutouts and the mesh is without them (warning in `cut_warnings`).
pub(crate) fn shape_mesh_ex(db: &Connection, id: i64) -> Result<Option<(ShapeMesh, bool)>, Error> {
    let row: Option<(i64, String)> = db
        .query_row(
            "SELECT mode, data FROM slab_shape WHERE slab=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((mode, data)) = row else {
        return Ok(None);
    };
    let mode = if mode == 1 {
        Thickness::Constant
    } else {
        Thickness::Varying
    };
    let s = parse_shape(&data, mode)?;
    let rings = slab_rings(db, id)?;
    let m0 = s
        .build(&rings)
        .map_err(|e| Error::Geom(format!("slab {id} shape: {e:?}")))?;
    let Some(cut) = crate::cuts::slab_cut(db, id)? else {
        return Ok(Some((m0, true)));
    };
    if cut.applied.is_empty() {
        return Ok(Some((m0, true)));
    }
    Ok(Some(match crate::cuts::cut_shape(&s, &m0, &cut.pieces) {
        Some(m) => (m, true),
        None => (m0, false),
    }))
}

/// The sketch rings (after tessellation) match the slab's current outline.
fn sketch_matches(rs: &[ddc_geom::sketch::Region], cur: &[Vec<V2>]) -> bool {
    let Some(r) = rs.first().filter(|_| rs.len() == 1) else {
        return false;
    };
    let rings = r.rings(SKETCH_SAG);
    rings.len() == cur.len()
        && rings.iter().zip(cur).all(|(a, b)| {
            // order and winding may be normalised on write, so compare as sets
            a.len() == b.len() && a.iter().all(|p| b.iter().any(|q| (*p - *q).len() < 1e-6))
        })
}

impl Model {
    /// "Finish" in sketch mode: validation, writing the curves and (for a slab) the new outline,
    /// in one journal operation = one undo step. Sketch errors are `Invalid` with a list.
    pub fn finish_sketch(&mut self, host: SketchHost, sk: &Sketch) -> Result<(), Error> {
        self.finish_sketch_with(host, sk, &[], &[]).map(|_| ())
    }

    /// `finish_sketch` plus edges linked to walls (`boundary_link`): `links` added, edges in
    /// `release` unlinked; returns how many links were released.
    pub(crate) fn finish_sketch_with(
        &mut self,
        host: SketchHost,
        sk: &Sketch,
        links: &[crate::boundary_link::EdgeLink],
        release: &[usize],
    ) -> Result<usize, Error> {
        let regions = sk
            .validate()
            .map_err(|e| Error::Invalid(format!("sketch: {e:?}")))?;
        let (kind, id) = host.key();
        if let SketchHost::HostOpening(_) = host {
            crate::cuts::opening_ring(sk)?;
        }
        let one = |what: &str| -> Result<(), Error> {
            if regions.len() != 1 {
                return invalid(format!(
                    "{what} sketch must be one region, got {}",
                    regions.len()
                ));
            }
            Ok(())
        };
        match host {
            SketchHost::Shaft(_) => {
                one("shaft")?;
                if !regions[0].holes.is_empty() {
                    return invalid("shaft sketch must have no holes".into());
                }
            }
            SketchHost::Ceiling(_) => one("ceiling")?,
            _ => {}
        }
        let text = sketch_text(sk);
        // by host kind: an outline edit recomputes quantities (`derives`)
        let op = format!("{kind}_sketch");
        self.op(&op, |db| {
            if !exists(db, kind, id)? {
                return invalid(format!("no such {kind} {id}"));
            }
            let mut inv = Inv::default().sql(
                "DELETE FROM sketch WHERE host_kind=?1 AND host=?2",
                vec![kind.to_string().into(), id.into()],
            );
            {
                let mut st =
                    db.prepare("SELECT data FROM sketch WHERE host_kind=?1 AND host=?2")?;
                if let Some(old) = st
                    .query_row(params![kind, id], |r| r.get::<_, String>(0))
                    .optional()?
                {
                    inv = inv.sql(
                        "INSERT INTO sketch(host_kind, host, data) VALUES (?1, ?2, ?3)",
                        vec![kind.to_string().into(), id.into(), old.into()],
                    );
                }
            }
            db.execute(
                "INSERT OR REPLACE INTO sketch(host_kind, host, data) VALUES (?1, ?2, ?3)",
                params![kind, id, text],
            )?;
            if let SketchHost::Slab(_) = host {
                if regions.len() != 1 {
                    return invalid(format!(
                        "slab sketch must be one region, got {}",
                        regions.len()
                    ));
                }
                let rings = regions[0].rings(SKETCH_SAG);
                let pts = |r: &Vec<V2>| r.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
                let holes: Vec<Vec<(f64, f64)>> = rings[1..].iter().map(pts).collect();
                let old = crate::slabs::rings(db, id)?;
                let p = crate::slabs::profile_keeping(&pts(&rings[0]), &holes, &old)?;
                inv = inv
                    .delete("slab_vertex", "slab", id)
                    .restore(db, "slab_vertex", "slab", id)?
                    .delete("slab_shape", "slab", id)
                    .restore(db, "slab_shape", "slab", id)?;
                db.execute("DELETE FROM slab_vertex WHERE slab=?1", [id])?;
                // the shape is tied to the old outline's vertices, so it is reset
                db.execute("DELETE FROM slab_shape WHERE slab=?1", [id])?;
                crate::slabs::write_rings(db, id, &p)?;
            }
            // shaft and ceiling: the outline comes from the sketch in the same operation
            let pts = |r: &Vec<V2>| r.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
            if let SketchHost::Shaft(_) = host {
                let ring = pts(&regions[0].rings(SKETCH_SAG)[0]);
                let old = vec![crate::shafts::ring_of(db, id)?];
                crate::slabs::profile_keeping(&ring, &[], &old)?;
                inv = inv.delete("shaft_vertex", "shaft", id).restore(
                    db,
                    "shaft_vertex",
                    "shaft",
                    id,
                )?;
                crate::shafts::write_shaft_ring(db, id, &ring)?;
            }
            if let SketchHost::Ceiling(_) = host {
                let rings = regions[0].rings(SKETCH_SAG);
                let holes: Vec<Vec<(f64, f64)>> = rings[1..].iter().map(pts).collect();
                let old = crate::shafts::ceiling_rings(db, id)?;
                let p = crate::slabs::profile_keeping(&pts(&rings[0]), &holes, &old)?;
                inv = inv.delete("ceiling_vertex", "ceiling", id).restore(
                    db,
                    "ceiling_vertex",
                    "ceiling",
                    id,
                )?;
                crate::shafts::write_ceiling_rings(db, id, &p)?;
            }
            if let SketchHost::HostOpening(_) = host {
                crate::cuts::check_in_host(db, id, &crate::cuts::opening_ring(sk)?)?;
            }
            let (inv, released) =
                crate::boundary_link::on_finish(db, inv, (kind, id), sk, links, release)?;
            Ok((released, id, inv))
        })
    }

    pub fn sketch_of(&self, host: SketchHost) -> Result<Option<Sketch>, Error> {
        let (kind, id) = host.key();
        let t: Option<String> = self
            .db
            .query_row(
                "SELECT data FROM sketch WHERE host_kind=?1 AND host=?2",
                params![kind, id],
                |r| r.get(0),
            )
            .optional()?;
        t.map(|t| parse_sketch(&t)).transpose()
    }

    /// Slab shape (None clears it). Validated by building it over the current outline.
    pub fn set_slab_shape(&mut self, id: i64, shape: Option<&SlabShape>) -> Result<(), Error> {
        let rings = slab_rings(&self.db, id)?;
        if let Some(s) = shape {
            let (_, t) = slab_top_t(&self.db, id)?;
            s.build(&rings)
                .and_then(|m| m.check_thickness(t))
                .map_err(|e| Error::Invalid(format!("slab {id} shape: {e:?}")))?;
        }
        let row = shape.filter(|s| !s.is_empty()).map(|s| {
            (
                if s.mode == Thickness::Constant { 1 } else { 0 },
                shape_text(s),
            )
        });
        self.op("slab_shape", |db| {
            if !exists(db, "slab", id)? {
                return invalid(format!("no such slab {id}"));
            }
            let inv = Inv::default().delete("slab_shape", "slab", id).restore(
                db,
                "slab_shape",
                "slab",
                id,
            )?;
            db.execute("DELETE FROM slab_shape WHERE slab=?1", [id])?;
            if let Some((mode, text)) = &row {
                db.execute(
                    "INSERT INTO slab_shape(slab, mode, data) VALUES (?1, ?2, ?3)",
                    params![id, mode, text],
                )?;
            }
            Ok(((), id, inv))
        })
    }

    pub fn slab_shape(&self, id: i64) -> Result<Option<SlabShape>, Error> {
        let row: Option<(i64, String)> = self
            .db
            .query_row(
                "SELECT mode, data FROM slab_shape WHERE slab=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(m, d)| {
            parse_shape(
                &d,
                if m == 1 {
                    Thickness::Constant
                } else {
                    Thickness::Varying
                },
            )
        })
        .transpose()
    }

    pub fn slab_shape_mesh(&self, id: i64) -> Result<Option<ShapeMesh>, Error> {
        shape_mesh(&self.db, id)
    }

    /// (volume, true top area) taking the shape into account; without a shape the top is flat.
    pub fn slab_shape_quantities(&self, id: i64) -> Result<(f64, f64), Error> {
        let (_, t) = slab_top_t(&self.db, id)?;
        match shape_mesh(&self.db, id)? {
            Some(m) => Ok((m.volume(t), m.top_area())),
            None => {
                // outline from the sketch: the area is exact for arcs, ellipses and splines
                // the sketch is stale if the outline was edited after "Finish"; then use the vertices
                let cur = slab_rings(&self.db, id)?;
                if let Some(sk) = self.sketch_of(SketchHost::Slab(id))? {
                    if let Some(rs) = sk.validate().ok().filter(|rs| sketch_matches(rs, &cur)) {
                        let a: f64 = rs.iter().map(|r| r.area()).sum();
                        return Ok((a * t, a));
                    }
                }
                let m = SlabShape::default()
                    .build(&cur)
                    .map_err(|e| Error::Geom(format!("{e:?}")))?;
                Ok((m.volume(t), m.top_area()))
            }
        }
    }

    /// Wall profile on the elevation (None clears it): the wall is not rectangular.
    pub fn set_wall_profile(
        &mut self,
        wall: i64,
        p: Option<&ElevationProfile>,
    ) -> Result<(), Error> {
        crate::wall_spline::refuse_piece(&self.db, wall)?;
        if let Some(p) = p {
            p.validate()
                .map_err(|e| Error::Invalid(format!("wall {wall} profile: {e:?}")))?;
            let len = self.wall_length(wall)?;
            let out = p.outer.iter().chain(p.holes.iter().flatten());
            if out.into_iter().any(|q| q.0 < -1e-9 || q.0 > len + 1e-9) {
                return invalid(format!("wall {wall} profile outside 0..{len}"));
            }
            // holes in the wall are openings (doors/windows): they have infill, a mark and an IFC cutout
            if !p.holes.is_empty() {
                return invalid(format!("wall {wall} profile: holes go as wall openings"));
            }
        }
        let text = p.map(profile_text);
        self.op("wall_profile", |db| {
            if !exists(db, "wall", wall)? {
                return invalid(format!("no such wall {wall}"));
            }
            let inv = Inv::default()
                .delete("wall_profile", "wall", wall)
                .restore(db, "wall_profile", "wall", wall)?
                .restore(db, "wall", "id", wall)?;
            // the top is cleared only if the previous profile drove it
            let had: bool = db.query_row(
                "SELECT count(*) > 0 FROM wall_profile WHERE wall=?1",
                [wall],
                |r| r.get(0),
            )?;
            db.execute("DELETE FROM wall_profile WHERE wall=?1", [wall])?;
            if let Some(t) = &text {
                db.execute("UPDATE wall SET top_roof=NULL, top_core=NULL, top_auto=0 WHERE id=?1", [wall])?;
                db.execute(
                    "INSERT INTO wall_profile(wall, data) VALUES (?1, ?2)",
                    params![wall, t],
                )?;
            }
            crate::cuts::sync_wall(db, wall, had)?;
            Ok(((), wall, inv))
        })
    }

    pub fn wall_profile(&self, wall: i64) -> Result<Option<ElevationProfile>, Error> {
        let t: Option<String> = self
            .db
            .query_row("SELECT data FROM wall_profile WHERE wall=?1", [wall], |r| {
                r.get(0)
            })
            .optional()?;
        t.map(|t| parse_profile(&t)).transpose()
    }

    /// Attaches the wall bottom (to a slab top) or top (to a slab bottom); follows the slab's shape.
    pub fn attach_wall_to_slab(
        &mut self,
        wall: i64,
        side: WallSide,
        slab: i64,
    ) -> Result<(), Error> {
        crate::wall_spline::refuse_piece(&self.db, wall)?;
        self.op("wall_attach_slab", |db| {
            if !exists(db, "wall", wall)? || !exists(db, "slab", slab)? {
                return invalid(format!("no such wall {wall} or slab {slab}"));
            }
            let inv = Inv::default()
                .sql(
                    "DELETE FROM wall_attach WHERE wall=?1 AND side=?2",
                    vec![wall.into(), side.key().to_string().into()],
                )
                .restore(db, "wall_attach", "wall", wall)?;
            db.execute(
                "INSERT OR REPLACE INTO wall_attach(wall, side, slab) VALUES (?1, ?2, ?3)",
                params![wall, side.key(), slab],
            )?;
            let mut inv = inv.restore(db, "wall", "id", wall)?;
            if side == WallSide::Top {
                // the top attaches to one target only: a slab displaces a roof and a level
                inv = inv.restore(db, "wall_attach_level", "wall", wall)?;
                db.execute("UPDATE wall SET top_roof=NULL, top_core=NULL, top_auto=0 WHERE id=?1", [wall])?;
                db.execute("DELETE FROM wall_attach_level WHERE wall=?1", [wall])?;
            }
            // a bottom attachment does not touch the top (roof, manual top profile)
            crate::cuts::sync_wall(db, wall, side == WallSide::Top)?;
            Ok(((), wall, inv))
        })
    }

    pub fn detach_wall_from_slab(&mut self, wall: i64, side: WallSide) -> Result<(), Error> {
        self.op("wall_detach_slab", |db| {
            let inv = Inv::default().delete("wall_attach", "wall", wall).restore(
                db,
                "wall_attach",
                "wall",
                wall,
            )?;
            db.execute(
                "DELETE FROM wall_attach WHERE wall=?1 AND side=?2",
                params![wall, side.key()],
            )?;
            let mut inv = inv.restore(db, "wall", "id", wall)?;
            if side == WallSide::Top {
                inv = inv.restore(db, "wall_attach_level", "wall", wall)?;
                db.execute("DELETE FROM wall_attach_level WHERE wall=?1", [wall])?;
            }
            crate::cuts::sync_wall(db, wall, side == WallSide::Top)?;
            Ok(((), wall, inv))
        })
    }

    /// Absolute (bottom, top) of a wall at points `us` along the axis: the bottom from the
    /// attachment/profile, the top from `top_profile` (written by attachments to a roof, slab,
    /// level, and by the profile).
    pub fn wall_extent_at(&self, wall: i64, us: &[f64]) -> Result<Vec<(f64, f64)>, Error> {
        let (base, h): (f64, f64) = self.db.query_row(
            "SELECT lv.elevation + w.base_offset, w.height FROM wall w \
             JOIN level lv ON lv.id = w.level WHERE w.id=?1",
            [wall],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let top = self.wall_top_profile(wall)?;
        let bp = crate::cuts::base_profile(&self.db, &mut crate::cuts::Shapes::default(), wall)?;
        Ok(us
            .iter()
            .map(|&u| {
                let z0 = base
                    + bp.as_deref()
                        .map_or(0.0, |b| ddc_geom::top::height_at(b, u));
                let z1 = base + top.as_deref().map_or(h, |t| ddc_geom::top::height_at(t, u));
                (z0, z1)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, RoofKind, RoofSpec, SlabSpec, WallSpec};
    use std::f64::consts::PI;

    fn setup() -> (Model, i64, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let st = m
            .add_slab_type(
                "floor",
                &[LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let wt = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        (m, lv, st, wt)
    }

    #[test]
    fn sketch_finish_is_one_undo_and_round_trips() {
        let (mut m, lv, st, _) = setup();
        let s = m
            .add_slab(&SlabSpec {
                level: lv,
                slab_type: st,
                offset: 0.0,
                outer: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)],
                holes: vec![],
            })
            .unwrap();
        // 6×4 with a semicircular end r=2 and a round hole r=0.5
        let sk = Sketch {
            curves: vec![
                SketchCurve::Line {
                    a: v2(0.0, 0.0),
                    b: v2(6.0, 0.0),
                },
                SketchCurve::arc3(v2(6.0, 0.0), v2(8.0, 2.0), v2(6.0, 4.0)).unwrap(),
                SketchCurve::Line {
                    a: v2(6.0, 4.0),
                    b: v2(0.0, 4.0),
                },
                SketchCurve::Line {
                    a: v2(0.0, 4.0),
                    b: v2(0.0, 0.0),
                },
                SketchCurve::Ellipse {
                    c: v2(3.0, 2.0),
                    rx: 0.5,
                    ry: 0.5,
                    rot: 0.0,
                    t0: 0.0,
                    sweep: 2.0 * PI,
                },
            ],
        };
        m.finish_sketch(SketchHost::Slab(s), &sk).unwrap();
        assert_eq!(m.sketch_of(SketchHost::Slab(s)).unwrap().unwrap(), sk);
        let exact = sk.validate().unwrap()[0].area();
        assert!((exact - (24.0 + 2.0 * PI - 0.25 * PI)).abs() < 1e-12);
        let (v, _) = m.slab_shape_quantities(s).unwrap();
        assert!((v - exact * 0.2).abs() < 1e-12, "{v}");
        // a bad sketch is rejected, nothing is written
        let open = Sketch {
            curves: sk.curves[..3].to_vec(),
        };
        assert!(m.finish_sketch(SketchHost::Slab(s), &open).is_err());
        m.undo().unwrap();
        assert!(m.sketch_of(SketchHost::Slab(s)).unwrap().is_none());
        let (v, _) = m.slab_shape_quantities(s).unwrap();
        assert!((v - 3.2).abs() < 1e-12);
    }

    #[test]
    fn drained_slab_volume_and_wall_base_follows() {
        let (mut m, lv, st, wt) = setup();
        let ring = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let s = m
            .add_slab(&SlabSpec {
                level: lv,
                slab_type: st,
                offset: 0.0,
                outer: ring.clone(),
                holes: vec![],
            })
            .unwrap();
        let outer: Vec<V2> = ring.iter().map(|&(x, y)| v2(x, y)).collect();
        m.set_slab_shape(s, Some(&SlabShape::drain(&outer, v2(5.0, 5.0), 0.06)))
            .unwrap();
        let (v, a) = m.slab_shape_quantities(s).unwrap();
        assert!((v - (20.0 - 2.0)).abs() < 1e-12, "{v}");
        assert!((a - 20.0 * 25.0036f64.sqrt()).abs() < 1e-12);
        // a wall along the diagonal "valley": the bottom sits on the top of the screed
        let w = m
            .add_wall(&WallSpec {
                a: (5.0, 5.0),
                b: (5.0, 10.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        m.attach_wall_to_slab(w, WallSide::Base, s).unwrap();
        let ex = m.wall_extent_at(w, &[0.0, 2.5, 5.0]).unwrap();
        assert!((ex[0].0 + 0.06).abs() < 1e-12);
        assert!((ex[1].0 + 0.03).abs() < 1e-12);
        assert!(ex[2].0.abs() < 1e-12);
        // the shape changes, the bottom follows
        m.set_slab_shape(s, Some(&SlabShape::drain(&outer, v2(5.0, 5.0), 0.1)))
            .unwrap();
        assert!((m.wall_extent_at(w, &[0.0]).unwrap()[0].0 + 0.1).abs() < 1e-12);
        m.undo().unwrap();
        assert!((m.wall_extent_at(w, &[0.0]).unwrap()[0].0 + 0.06).abs() < 1e-12);
    }

    #[test]
    fn wall_under_hip_roof_heights_at_points() {
        let (mut m, lv, st, wt) = setup();
        let r = m
            .add_roof(&RoofSpec {
                level: lv,
                slab_type: st,
                offset: 3.0,
                overhang: 0.0,
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: vec![(0.0, 0.0), (12.0, 0.0), (12.0, 8.0), (0.0, 8.0)],
                    slopes: vec![Some(30.0); 4],
                    mansard: None,
                },
            })
            .unwrap();
        // an interior wall at y=4 (under the ridge and hips)
        let w = m
            .add_wall(&WallSpec {
                a: (0.5, 4.0),
                b: (11.5, 4.0),
                level: lv,
                base_offset: 0.0,
                height: 2.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        m.attach_wall_top(w, r).unwrap();
        let (tn, k) = (30f64.to_radians().tan(), 0.2 / 30f64.to_radians().cos());
        // u=0 -> x=0.5 (hip: distance to the eave 0.5); u=5.5 -> x=6 (ridge, 4 m)
        let ex = m.wall_extent_at(w, &[0.0, 1.5, 5.5]).unwrap();
        for (e, d) in ex.iter().zip([0.5, 2.0, 4.0]) {
            assert_eq!(e.0, 0.0);
            assert!((e.1 - (3.0 + tn * d - k)).abs() < 1e-9, "{e:?} {d}");
        }
    }

    #[test]
    fn gable_wall_profile_stored_and_undone() {
        let (mut m, lv, _, wt) = setup();
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
        let p = ElevationProfile {
            outer: vec![(0.0, 0.0), (8.0, 0.0), (8.0, 3.0), (4.0, 5.0), (0.0, 3.0)],
            holes: vec![],
        };
        m.set_wall_profile(w, Some(&p)).unwrap();
        let got = m.wall_profile(w).unwrap().unwrap();
        assert_eq!(got, p);
        assert!((got.volume(0.2) - 32.0 * 0.2).abs() < 1e-12);
        let wide = ElevationProfile {
            outer: vec![(0.0, 0.0), (9.0, 0.0), (9.0, 3.0)],
            holes: vec![],
        };
        assert!(m.set_wall_profile(w, Some(&wide)).is_err());
        m.undo().unwrap();
        assert!(m.wall_profile(w).unwrap().is_none());
    }

    fn square_slab(m: &mut Model, lv: i64, st: i64) -> i64 {
        m.add_slab(&SlabSpec {
            level: lv,
            slab_type: st,
            offset: 0.0,
            outer: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)],
            holes: vec![],
        })
        .unwrap()
    }

    fn circle(r: f64) -> Sketch {
        Sketch {
            curves: vec![SketchCurve::Ellipse {
                c: v2(2.0, 2.0),
                rx: r,
                ry: r,
                rot: 0.0,
                t0: 0.0,
                sweep: 2.0 * PI,
            }],
        }
    }

    #[test]
    fn quantities_follow_boundary_edited_after_sketch() {
        let (mut m, lv, st, _) = setup();
        let s = square_slab(&mut m, lv, st);
        m.finish_sketch(SketchHost::Slab(s), &circle(1.0)).unwrap();
        let (_, a) = m.slab_shape_quantities(s).unwrap();
        assert!((a - PI).abs() < 1e-9, "{a}");
        m.set_slab_boundary(s, &[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)], &[])
            .unwrap();
        let (v, a) = m.slab_shape_quantities(s).unwrap();
        assert!((a - 4.0).abs() < 1e-9 && (v - 0.8).abs() < 1e-9, "{a} {v}");
    }

    #[test]
    fn host_delete_cascades_and_undo_restores() {
        let (mut m, lv, st, wt) = setup();
        let s = square_slab(&mut m, lv, st);
        m.finish_sketch(SketchHost::Slab(s), &circle(1.5)).unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (1.0, 2.0),
                b: (3.0, 2.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        m.attach_wall_to_slab(w, WallSide::Base, s).unwrap();
        m.delete_slab(s).unwrap();
        m.assert_integrity();
        assert!(m.sketch_of(SketchHost::Slab(s)).unwrap().is_none());
        // no dangling attachment, the wall heights are readable
        assert_eq!(m.wall_extent_at(w, &[0.0, 2.0]).unwrap().len(), 2);
        m.undo().unwrap();
        m.assert_integrity();
        assert_eq!(
            m.sketch_of(SketchHost::Slab(s)).unwrap().unwrap(),
            circle(1.5)
        );
        let n: i64 =
            m.db.query_row("SELECT count(*) FROM wall_attach WHERE slab=?1", [s], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);
        m.delete_wall(w).unwrap();
        m.assert_integrity();
        m.undo().unwrap();
        m.assert_integrity();
    }

    #[test]
    fn drain_deeper_than_slab_is_rejected() {
        let (mut m, lv, st, _) = setup();
        let s = square_slab(&mut m, lv, st);
        let mut sh = SlabShape {
            points: vec![(v2(2.0, 2.0), -0.25)],
            ..Default::default()
        };
        let e = m.set_slab_shape(s, Some(&sh)).unwrap_err();
        assert!(e.to_string().contains("TooDeep"), "{e}");
        assert!(m.slab_shape(s).unwrap().is_none());
        sh.points[0].1 = -0.1;
        m.set_slab_shape(s, Some(&sh)).unwrap();
        sh.mode = Thickness::Constant;
        sh.points[0].1 = -0.5;
        m.set_slab_shape(s, Some(&sh)).unwrap();
    }
}
