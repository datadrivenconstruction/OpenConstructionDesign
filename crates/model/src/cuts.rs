// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Host cut-outs and wall top/bottom from profiles and attachments.
//! `host_opening` is a sketch-based opening in a slab, roof or ceiling (the outline lives in `sketch` as
//! host `host_opening`; `opening` there is a wall opening). Shafts and openings are subtracted from the host
//! plan in one boolean operation (`cut_region`): its result is read by the body, quantities,
//! views, sections and IFC. The wall top from `wall_profile` / `wall_attach` (slab) /
//! `wall_attach_level` is written to `wall.top_profile`; the bottom is `base_profile` (dz above the wall base).

use std::collections::HashMap;

use ddc_geom::math::{v2, V2};
use ddc_geom::poly;
use ddc_geom::shape_edit::{ShapeMesh, SlabShape, Thickness};
use ddc_geom::sketch::Sketch;
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::sketch::{parse_sketch, sketch_text, SKETCH_SAG};
use crate::{ElementWarning, Error, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS host_opening(id INTEGER PRIMARY KEY, host_kind TEXT NOT NULL,
    host INTEGER NOT NULL, dormer INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS wall_attach_level(wall INTEGER PRIMARY KEY, level INTEGER NOT NULL,
    offset REAL NOT NULL);
";

/// Host-opening sketches stored under the `opening` key (used by wall openings): a reference to
/// `host_opening` without a same-named wall opening is redirected to `host_opening`.
pub(crate) fn migrate_sketch_kind(db: &Connection) -> Result<(), Error> {
    db.execute(
        "UPDATE sketch SET host_kind='host_opening' WHERE host_kind='opening' \
         AND host IN (SELECT id FROM host_opening) AND host NOT IN (SELECT id FROM opening)",
        [],
    )?;
    Ok(())
}

/// Host of a sketch-based opening.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpeningHost {
    Slab(i64),
    Roof(i64),
    Ceiling(i64),
}

impl OpeningHost {
    pub(crate) fn key(self) -> (&'static str, i64) {
        match self {
            OpeningHost::Slab(i) => ("slab", i),
            OpeningHost::Roof(i) => ("roof", i),
            OpeningHost::Ceiling(i) => ("ceiling", i),
        }
    }
    fn parse(k: &str, id: i64) -> Option<Self> {
        Some(match k {
            "slab" => OpeningHost::Slab(id),
            "roof" => OpeningHost::Roof(id),
            "ceiling" => OpeningHost::Ceiling(id),
            _ => return None,
        })
    }
}

/// Host opening: plan outline counter-clockwise (arcs subdivided).
#[derive(Clone, Debug, PartialEq)]
pub struct HostOpening {
    pub id: i64,
    pub host: OpeningHost,
    /// Dormer: cuts only the roof slopes (a plain opening also cuts horizontal platforms).
    pub dormer: bool,
    pub ring: Vec<(f64, f64)>,
}

fn invalid<T>(m: String) -> Result<T, Error> {
    Err(Error::Invalid(m))
}

fn exists(db: &Connection, table: &str, id: i64) -> Result<bool, Error> {
    let n: i64 = db.query_row(
        &format!("SELECT count(*) FROM {table} WHERE id=?1"),
        [id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn xy(r: &[(f64, f64)]) -> Vec<V2> {
    r.iter().map(|p| v2(p.0, p.1)).collect()
}

fn tup(r: &[V2]) -> Vec<(f64, f64)> {
    r.iter().map(|p| (p.x, p.y)).collect()
}

/// Opening outline: the sketch must be exactly one region without holes.
pub(crate) fn opening_ring(sk: &Sketch) -> Result<Vec<V2>, Error> {
    let rs = sk
        .validate()
        .map_err(|e| Error::Invalid(format!("opening sketch: {e:?}")))?;
    if rs.len() != 1 || !rs[0].holes.is_empty() {
        return invalid("opening sketch must be one loop without holes".into());
    }
    let mut r = rs[0].outer.ring(SKETCH_SAG);
    if poly::area(&r) < 0.0 {
        r.reverse();
    }
    Ok(r)
}

/// Openings of host `kind id` (counter-clockwise); invalid sketches are skipped.
pub(crate) fn openings_of(db: &Connection, kind: &str, id: i64) -> Result<Vec<HostOpening>, Error> {
    let mut st = db.prepare_cached(
        "SELECT o.id, o.dormer, s.data FROM host_opening o JOIN sketch s \
         ON s.host_kind='host_opening' AND s.host=o.id WHERE o.host_kind=?1 AND o.host=?2 \
         ORDER BY o.id",
    )?;
    let rows: Vec<(i64, i64, String)> = st
        .query_map(params![kind, id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let mut out = vec![];
    for (oid, dormer, data) in rows {
        let Some(ring) = parse_sketch(&data).ok().and_then(|s| opening_ring(&s).ok()) else {
            continue;
        };
        out.push(HostOpening {
            id: oid,
            host: OpeningHost::parse(kind, id).expect("known kind"),
            dormer: dormer != 0,
            ring: tup(&ring),
        });
    }
    Ok(out)
}

/// Host plan after cuts.
#[derive(Clone, Debug, Default)]
pub(crate) struct Cut {
    /// Pieces: outer ring counter-clockwise, holes clockwise (notches are part of the outer ring).
    pub pieces: Vec<(Vec<V2>, Vec<Vec<V2>>)>,
    /// Applied cuts (counter-clockwise outlines), for plan symbols.
    pub applied: Vec<Vec<V2>>,
    /// Skipped cuts: (table, id, reason).
    pub skipped: Vec<(&'static str, i64, String)>,
}

impl Cut {
    pub fn area(&self) -> f64 {
        poly::pieces_area(&self.pieces)
    }
    /// All piece rings in sequence (for even-odd fill).
    pub fn rings(&self) -> Vec<Vec<V2>> {
        self.pieces
            .iter()
            .flat_map(|(o, hs)| std::iter::once(o.clone()).chain(hs.iter().cloned()))
            .collect()
    }
}

fn valid_pieces(p: &[(Vec<V2>, Vec<Vec<V2>>)]) -> bool {
    !p.is_empty()
        && p.iter().all(|(o, hs)| {
            // saved spikes stay drawable; new outlines are checked strictly where written
            let tol = ddc_geom::Tol::default();
            ddc_geom::Profile::new_keeping_spikes(o.clone(), hs.clone(), tol).is_ok()
        })
}

/// Area of region `host` (outer ring + holes) covered by outline `ring`.
fn overlap(host: &[Vec<V2>], ring: &[V2]) -> f64 {
    let pieces = poly::convex_pieces(ring);
    host.iter()
        .enumerate()
        .map(|(k, r)| {
            let a = poly::overlap_area(r, &pieces);
            // outer ring counter-clockwise, holes clockwise (subtracted)
            let s = if (poly::area(r) > 0.0) == (k == 0) {
                1.0
            } else {
                -1.0
            };
            a * s
        })
        .sum()
}

/// Plan of host `kind id` (`host` is [outer, holes…]) minus shafts crossing
/// (z0, z1) and sketch-based openings. A cut that breaks the result is skipped with a warning.
pub(crate) fn cut_region(
    db: &Connection,
    kind: &str,
    id: i64,
    host: &[Vec<V2>],
    z0: f64,
    z1: f64,
) -> Result<Cut, Error> {
    let Some(outer) = host.first() else {
        return Ok(Cut::default());
    };
    let (lo, hi) = bbox(outer);
    let mut cands: Vec<(&'static str, i64, Vec<V2>)> = vec![];
    for (sid, ring) in crate::shafts::shaft_rings(db, z0, z1)? {
        let (a, b) = bbox(&ring);
        if a.x < hi.x && b.x > lo.x && a.y < hi.y && b.y > lo.y {
            cands.push(("shaft", sid, ring));
        }
    }
    for o in openings_of(db, kind, id)? {
        cands.push(("host_opening", o.id, xy(&o.ring)));
    }
    let mut out = Cut::default();
    let mut live = vec![];
    for (t, cid, ring) in cands {
        if overlap(host, &ring) <= 1e-9 {
            if t == "host_opening" {
                out.skipped
                    .push((t, cid, format!("outside its {kind} {id}")));
            }
            continue;
        }
        live.push((t, cid, ring));
    }
    let base = vec![(
        ccw(host[0].clone()),
        host[1..].iter().map(|h| cw(h.clone())).collect::<Vec<_>>(),
    )];
    if live.is_empty() {
        out.pieces = base;
        return Ok(out);
    }
    let all: Vec<Vec<V2>> = live.iter().map(|c| c.2.clone()).collect();
    match poly::subtract(host, &all) {
        Some(p) if valid_pieces(&p) => {
            out.pieces = p;
            out.applied = all;
        }
        _ => {
            // one at a time: a cut after which the plan cannot be built is skipped
            out.pieces = base;
            for (t, cid, ring) in live {
                let mut cur = out.applied.clone();
                cur.push(ring.clone());
                match poly::subtract(host, &cur).filter(|p| valid_pieces(p)) {
                    Some(p) => {
                        out.pieces = p;
                        out.applied = cur;
                    }
                    None => {
                        out.skipped
                            .push((t, cid, format!("cut of {kind} {id} not built, skipped")))
                    }
                }
            }
        }
    }
    Ok(out)
}

fn bbox(r: &[V2]) -> (V2, V2) {
    r.iter().fold(
        (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN)),
        |(l, h), p| {
            (
                v2(l.x.min(p.x), l.y.min(p.y)),
                v2(h.x.max(p.x), h.y.max(p.y)),
            )
        },
    )
}

/// Slab plan after cuts; None if there is no outline.
pub(crate) fn slab_cut(db: &Connection, id: i64) -> Result<Option<Cut>, Error> {
    let rs: Vec<Vec<V2>> = crate::slabs::rings(db, id)?.iter().map(|r| xy(r)).collect();
    if rs.is_empty() {
        return Ok(None);
    }
    let (top, t) = crate::sketch::slab_top_t(db, id)?;
    cut_region(db, "slab", id, &rs, top - t, top).map(Some)
}

/// Ceiling plan after cuts; None if there is no outline.
pub(crate) fn ceiling_cut(db: &Connection, id: i64) -> Result<Option<Cut>, Error> {
    let rs: Vec<Vec<V2>> = crate::shafts::ceiling_rings(db, id)?
        .iter()
        .map(|r| xy(r))
        .collect();
    if rs.is_empty() {
        return Ok(None);
    }
    let (z0, z1) = crate::shafts::ceiling_z(db, id)?;
    cut_region(db, "ceiling", id, &rs, z0, z1).map(Some)
}

/// Shape of slab `s` (mesh `m0` without cuts) over the plan pieces: piece vertex dz comes from the
/// original surface, shape points and lines stay inside their own piece. None if it could not be built.
pub(crate) fn cut_shape(
    s: &SlabShape,
    m0: &ShapeMesh,
    pieces: &[(Vec<V2>, Vec<Vec<V2>>)],
) -> Option<ShapeMesh> {
    let mut out = ShapeMesh {
        pts: vec![],
        dz: vec![],
        tris: vec![],
        mode: m0.mode,
    };
    for (o, hs) in pieces {
        let mut rings = vec![o.clone()];
        rings.extend(hs.iter().cloned());
        let whole = |a: V2, b: V2| {
            let iv = poly::inside_intervals(a, b, &rings);
            iv.len() == 1 && iv[0].0 < 1e-9 && iv[0].1 > 1.0 - 1e-9
        };
        let sp = SlabShape {
            ring_dz: rings
                .iter()
                .map(|r| r.iter().map(|p| m0.dz_at(*p).unwrap_or(0.0)).collect())
                .collect(),
            points: s
                .points
                .iter()
                .filter(|(p, _)| poly::contains(&rings, *p))
                .cloned()
                .collect(),
            lines: s
                .lines
                .iter()
                .filter(|l| whole(l.a, l.b))
                .cloned()
                .collect(),
            mode: s.mode,
        };
        let m = sp.build(&rings).ok()?;
        let k = out.pts.len();
        out.pts.extend(m.pts);
        out.dz.extend(m.dz);
        out.tris.extend(m.tris.iter().map(|t| t.map(|i| i + k)));
    }
    Some(out)
}

/// An opening must lie within the host outline (touching the edge makes a notch).
pub(crate) fn check_in_host(db: &Connection, opening: i64, ring: &[V2]) -> Result<(), Error> {
    let (k, h): (String, i64) = db.query_row(
        "SELECT host_kind, host FROM host_opening WHERE id=?1",
        [opening],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    check_ring_in(db, &k, h, ring)
}

fn check_ring_in(db: &Connection, kind: &str, host: i64, ring: &[V2]) -> Result<(), Error> {
    let a = poly::area(ring).abs();
    let covered = match kind {
        "slab" => crate::slabs::rings(db, host)?
            .first()
            .map_or(0.0, |o| overlap(&[ccw(xy(o))], ring)),
        "ceiling" => crate::shafts::ceiling_rings(db, host)?
            .first()
            .map_or(0.0, |o| overlap(&[ccw(xy(o))], ring)),
        "roof" => {
            let s = crate::roofs::load(db, host)?;
            crate::roof_edge::shape(db, host, &s)?
                .faces
                .iter()
                .filter(|f| f.sloped)
                .map(|f| {
                    let p: Vec<V2> = f.pts.iter().map(|q| v2(q.x, q.y)).collect();
                    overlap(&[ccw(p)], ring)
                })
                .sum()
        }
        _ => 0.0,
    };
    if covered < a - 1e-6 * a.max(1.0) {
        let kind: &'static str = match kind {
            "slab" => "slab",
            "ceiling" => "ceiling",
            _ => "roof",
        };
        return Err(Error::OpeningOutsideHost { kind, host });
    }
    Ok(())
}

fn ccw(mut r: Vec<V2>) -> Vec<V2> {
    if poly::area(&r) < 0.0 {
        r.reverse();
    }
    r
}

fn cw(mut r: Vec<V2>) -> Vec<V2> {
    if poly::area(&r) > 0.0 {
        r.reverse();
    }
    r
}

/// Openings are deleted together with their host (undo reinserts them).
pub(crate) fn drop_openings(
    db: &Connection,
    mut inv: Inv,
    kind: &str,
    id: i64,
) -> Result<Inv, Error> {
    let ids: Vec<i64> = {
        let mut st = db.prepare("SELECT id FROM host_opening WHERE host_kind=?1 AND host=?2")?;
        let v = st
            .query_map(params![kind, id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        v
    };
    for o in ids {
        inv = inv.restore(db, "host_opening", "id", o)?;
        inv = crate::sketch::drop_owned(db, inv, "host_opening", o)?;
        db.execute("DELETE FROM host_opening WHERE id=?1", [o])?;
    }
    Ok(inv)
}

/// Openings without a host (a polymorphic reference, outside the shared reference table).
pub(crate) fn orphans(db: &Connection) -> Result<Vec<crate::integrity::Issue>, Error> {
    let mut st = db.prepare("SELECT id, host_kind, host FROM host_opening ORDER BY id")?;
    let rows: Vec<(i64, String, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let mut out = vec![];
    for (id, k, h) in rows {
        let ok = match OpeningHost::parse(&k, h) {
            Some(_) => exists(db, &k, h)?,
            None => false,
        };
        if !ok {
            out.push(crate::integrity::Issue {
                table: "host_opening".into(),
                id,
                problem: format!("host -> missing {k} {h}"),
            });
        }
    }
    Ok(out)
}

// ---------- wall top and bottom ----------

/// Slab shapes already built in this pass (one build per slab).
#[derive(Default)]
pub(crate) struct Shapes(HashMap<i64, Option<ShapeMesh>>);

impl Shapes {
    fn get(&mut self, db: &Connection, slab: i64) -> Option<&ShapeMesh> {
        self.0
            .entry(slab)
            .or_insert_with(|| crate::sketch::shape_mesh(db, slab).ok().flatten())
            .as_ref()
    }
}

/// Straight wall: (a, b, absolute bottom, height).
fn wall_frame(db: &Connection, wall: i64) -> Result<Option<(V2, V2, f64, f64)>, Error> {
    let r: Option<(f64, f64, f64, f64, f64, f64, f64)> = db
        .query_row(
            "SELECT w.ax, w.ay, w.bx, w.by, lv.elevation + w.base_offset, w.height, w.bulge \
             FROM wall w JOIN level lv ON lv.id = w.level WHERE w.id=?1",
            [wall],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()?;
    Ok(r.filter(|r| r.6 == 0.0)
        .map(|r| (v2(r.0, r.1), v2(r.2, r.3), r.4, r.5)))
}

/// Envelope of a (u, z) polygon: upper (`upper`) or lower, with vertical steps.
pub(crate) fn envelope(ring: &[(f64, f64)], upper: bool) -> Vec<(f64, f64)> {
    let n = ring.len();
    let mut us: Vec<f64> = ring.iter().map(|p| p.0).collect();
    us.sort_by(f64::total_cmp);
    us.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
    let pick = |a: f64, b: f64| if upper { a.max(b) } else { a.min(b) };
    let init = if upper {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    };
    let mut out: Vec<(f64, f64)> = vec![];
    for (k, &u) in us.iter().enumerate() {
        let (mut left, mut right) = (init, init);
        for i in 0..n {
            let (p, q) = (ring[i], ring[(i + 1) % n]);
            let (lo, hi) = if p.0 < q.0 { (p, q) } else { (q, p) };
            if hi.0 - lo.0 < 1e-12 {
                continue;
            }
            let z = lo.1 + (hi.1 - lo.1) * (u - lo.0) / (hi.0 - lo.0);
            if lo.0 < u - 1e-12 && u <= hi.0 + 1e-12 {
                left = pick(left, z);
            }
            if lo.0 - 1e-12 <= u && u < hi.0 - 1e-12 {
                right = pick(right, z);
            }
        }
        let (first, last) = (k == 0, k + 1 == us.len());
        if first {
            out.push((u, right));
        } else if last {
            out.push((u, left));
        } else {
            out.push((u, left));
            if (right - left).abs() > 1e-9 {
                out.push((u, right));
            }
        }
    }
    out.retain(|p| p.1.is_finite());
    // breakpoints taken from the other side of the ring leave collinear points here
    let mut i = 1;
    while i + 1 < out.len() {
        let (a, b, c) = (out[i - 1], out[i], out[i + 1]);
        let cross = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
        if cross.abs() < 1e-9 && (b.0 - a.0) * (c.0 - b.0) + (b.1 - a.1) * (c.1 - b.1) > 0.0 {
            out.remove(i);
        } else {
            i += 1;
        }
    }
    out
}

/// Slab surface along a→b: (u, absolute z) of the top or bottom; outside the plan, the edge value is used.
fn slab_along(
    db: &Connection,
    sh: &mut Shapes,
    slab: i64,
    a: V2,
    b: V2,
    top: bool,
) -> Result<Vec<(f64, f64)>, Error> {
    let (z, t, slope): (f64, f64, Option<String>) = db.query_row(
        "SELECT lv.elevation + s.offset, (SELECT COALESCE(SUM(thickness),0) FROM slab_type_layer \
         WHERE stype=s.stype), s.slope FROM slab s JOIN level lv ON lv.id=s.level WHERE s.id=?1",
        [slab],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let len = (b - a).len();
    let plane = slope.and_then(|t| crate::slabs::parse_slope(&t).ok());
    let rise = |u: f64| plane.map_or(0.0, |p| p.rise(a + (b - a) * (u / len.max(1e-12))));
    let shape = sh.get(db, slab);
    let pts: Vec<(f64, f64)> = match shape {
        Some(m) => {
            let v = m.along(a, b);
            if v.is_empty() {
                vec![(0.0, 0.0), (len, 0.0)]
            } else {
                v
            }
        }
        None => vec![(0.0, 0.0), (len, 0.0)],
    };
    let varying = shape.is_some_and(|m| m.mode == Thickness::Varying);
    Ok(pts
        .into_iter()
        .map(|(u, dz)| {
            let zt = z + rise(u) + dz;
            let zz = if top {
                zt
            } else if varying {
                z + rise(u) - t
            } else {
                zt - t
            };
            (u, zz)
        })
        .collect())
}

fn attached(db: &Connection, wall: i64, side: &str) -> Result<Option<i64>, Error> {
    Ok(db
        .query_row(
            "SELECT slab FROM wall_attach WHERE wall=?1 AND side=?2",
            params![wall, side],
            |r| r.get(0),
        )
        .optional()?)
}

fn profile_of(
    db: &Connection,
    wall: i64,
) -> Result<Option<ddc_geom::sketch::ElevationProfile>, Error> {
    let t: Option<String> = db
        .query_row("SELECT data FROM wall_profile WHERE wall=?1", [wall], |r| {
            r.get(0)
        })
        .optional()?;
    t.map(|t| crate::sketch::parse_profile(&t)).transpose()
}

/// Polyline over the full length 0..len: the ends are extended at the extreme height (no jump at the wall end).
fn span(mut p: Vec<(f64, f64)>, len: f64) -> Vec<(f64, f64)> {
    if let Some(&f) = p.first() {
        if f.0 > 1e-9 {
            p.insert(0, (0.0, f.1));
        }
    }
    if let Some(&l) = p.last() {
        if l.0 < len - 1e-9 {
            p.push((len, l.1));
        }
    }
    p.retain(|q| q.0 >= -1e-9 && q.0 <= len + 1e-9);
    p
}

/// Wall top (u, h above the base) from profile/attachments; None if there are no sources.
fn derived_top(
    db: &Connection,
    sh: &mut Shapes,
    wall: i64,
) -> Result<Option<Vec<(f64, f64)>>, Error> {
    let Some((a, b, base, _)) = wall_frame(db, wall)? else {
        return Ok(None);
    };
    let len = (b - a).len();
    let lvl: Option<(f64, f64)> = db
        .query_row(
            "SELECT lv.elevation, x.offset FROM wall_attach_level x JOIN level lv ON lv.id=x.level \
             WHERE x.wall=?1",
            [wall],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((e, o)) = lvl {
        let h = e + o - base;
        return Ok(Some(vec![(0.0, h), (len, h)]));
    }
    if let Some(s) = attached(db, wall, "top")? {
        let v = slab_along(db, sh, s, a, b, false)?;
        return Ok(Some(span(
            v.into_iter().map(|(u, z)| (u, z - base)).collect(),
            len,
        )));
    }
    Ok(profile_of(db, wall)?.map(|p| span(envelope(&p.outer, true), len)))
}

fn base_of(db: &Connection, sh: &mut Shapes, wall: i64) -> Result<Option<Vec<(f64, f64)>>, Error> {
    let Some((a, b, base, _)) = wall_frame(db, wall)? else {
        return Ok(None);
    };
    let len = (b - a).len();
    let p = if let Some(s) = attached(db, wall, "base")? {
        slab_along(db, sh, s, a, b, true)?
            .into_iter()
            .map(|(u, z)| (u, z - base))
            .collect()
    } else if let Some(p) = profile_of(db, wall)? {
        envelope(&p.outer, false)
    } else {
        return Ok(None);
    };
    Ok(Some(span(p, len)).filter(|p: &Vec<(f64, f64)>| p.iter().any(|q| q.1.abs() > 1e-12)))
}

/// Wall bottom: polyline (u, dz above the wall base); None is flat. `sh` holds the slab shapes
/// of the pass (one build per slab for all walls).
pub(crate) fn base_profile(
    db: &Connection,
    sh: &mut Shapes,
    wall: i64,
) -> Result<Option<Vec<(f64, f64)>>, Error> {
    base_of(db, sh, wall)
}

/// A `top_roof` top is driven by the roof.
fn roof_led(db: &Connection, wall: i64) -> Result<bool, Error> {
    let roof: Option<Option<i64>> = db
        .query_row("SELECT top_roof FROM wall WHERE id=?1", [wall], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(roof.flatten().is_some())
}

/// Top from sources, checking "top above bottom" at the breakpoints of both top and bottom.
fn checked_top(
    db: &Connection,
    sh: &mut Shapes,
    wall: i64,
) -> Result<Option<Vec<(f64, f64)>>, Error> {
    let Some(t) = derived_top(db, sh, wall)? else {
        return Ok(None);
    };
    let base = base_of(db, sh, wall)?;
    let mut us: Vec<f64> = t.iter().map(|p| p.0).collect();
    us.extend(base.iter().flatten().map(|p| p.0));
    let bad = us.iter().any(|&u| {
        let z0 = base
            .as_deref()
            .map_or(0.0, |b| ddc_geom::top::height_at(b, u));
        ddc_geom::top::height_at(&t, u) <= z0 + 1e-6
    });
    if bad {
        return invalid(format!("wall {wall}: top would be at or below the base"));
    }
    Ok(Some(t))
}

/// Recomputes `wall.top_profile` from profile/attachments. `clear`: a removed source drove the top;
/// with no other sources the top is cleared; otherwise (a bottom edit) the manual top stays.
/// A `top_roof` top is left alone. The caller owns the wall-row snapshot for undo.
pub(crate) fn sync_wall(db: &Connection, wall: i64, clear: bool) -> Result<(), Error> {
    sync_wall_in(db, &mut Shapes::default(), wall, clear)
}

fn sync_wall_in(db: &Connection, sh: &mut Shapes, wall: i64, clear: bool) -> Result<(), Error> {
    if roof_led(db, wall)? {
        // the bottom is never above the top anyway
        let base = base_of(db, sh, wall)?;
        let top: Option<String> =
            db.query_row("SELECT top_profile FROM wall WHERE id=?1", [wall], |r| {
                r.get(0)
            })?;
        if let (Some(b), Some(t)) = (
            base,
            top.as_deref().and_then(crate::walls::parse_top_profile),
        ) {
            let mut us: Vec<f64> = t.iter().map(|p| p.0).collect();
            us.extend(b.iter().map(|p| p.0));
            if us
                .iter()
                .any(|&u| ddc_geom::top::height_at(&t, u) <= ddc_geom::top::height_at(&b, u) + 1e-6)
            {
                return invalid(format!("wall {wall}: base would be at or above the top"));
            }
        }
        return Ok(());
    }
    let top = checked_top(db, sh, wall)?;
    if top.is_none() && !clear {
        return Ok(());
    }
    let text = top.as_deref().map(crate::walls::top_profile_text);
    let cur: Option<String> =
        db.query_row("SELECT top_profile FROM wall WHERE id=?1", [wall], |r| {
            r.get(0)
        })?;
    if cur != text {
        db.execute(
            "UPDATE wall SET top_profile=?2 WHERE id=?1",
            params![wall, text],
        )?;
    }
    Ok(())
}

fn synced_walls(db: &Connection) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare(
        "SELECT wall FROM wall_attach WHERE side='top' UNION SELECT wall FROM wall_profile          UNION SELECT wall FROM wall_attach_level ORDER BY 1",
    )?;
    let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    Ok(v)
}

/// Before quantities are recomputed: walls attached to slabs/levels or with a profile
/// follow them (editing a slab changes the wall top in the same operation). Each slab's
/// shape is built once per pass.
pub(crate) fn sync_walls(db: &Connection) -> Result<(), Error> {
    let mut sh = Shapes::default();
    for w in synced_walls(db)? {
        if roof_led(db, w)? {
            continue;
        }
        match sync_wall_in(db, &mut sh, w, false) {
            // a top below the bottom after an edit does not fail the recompute: the wall stays as it was,
            // the warning goes to `cut_warnings`
            Ok(()) | Err(Error::Invalid(_)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Walls whose top does not follow the source (top below the bottom after an edit).
fn stale_walls(db: &Connection) -> Result<Vec<ElementWarning>, Error> {
    let mut sh = Shapes::default();
    let mut out = vec![];
    for w in synced_walls(db)? {
        if roof_led(db, w)? {
            continue;
        }
        if let Err(Error::Invalid(m)) = checked_top(db, &mut sh, w) {
            out.push(ElementWarning {
                table: "wall",
                id: w,
                message: m,
            });
        }
    }
    Ok(out)
}

/// Meshes concatenated (indices offset, faces preserved).
pub(crate) fn concat(ms: Vec<ddc_geom::mesh::Mesh>) -> ddc_geom::mesh::Mesh {
    let mut out = ddc_geom::mesh::Mesh::default();
    for m in ms {
        let o = out.positions.len() as u32;
        out.positions.extend(m.positions);
        out.triangles
            .extend(m.triangles.iter().map(|t| [t[0] + o, t[1] + o, t[2] + o]));
        out.face_of.extend(m.face_of);
    }
    out
}

fn pieces_xy(c: &Cut) -> Vec<(Vec<(f64, f64)>, Vec<Vec<(f64, f64)>>)> {
    c.pieces
        .iter()
        .map(|(o, hs)| (tup(o), hs.iter().map(|h| tup(h)).collect()))
        .collect()
}

impl Model {
    /// Sketch-based opening in a slab, roof or ceiling; `dormer` is a roof dormer.
    /// The outline must lie within the host outline (`Error::OpeningOutsideHost`).
    pub fn add_host_opening(
        &mut self,
        host: OpeningHost,
        sk: &Sketch,
        dormer: bool,
    ) -> Result<i64, Error> {
        let ring = opening_ring(sk)?;
        let (kind, hid) = host.key();
        let text = sketch_text(sk);
        self.op("host_opening_add", |db| {
            if !exists(db, kind, hid)? {
                return invalid(format!("no such {kind} {hid}"));
            }
            check_ring_in(db, kind, hid, &ring)?;
            db.execute(
                "INSERT INTO host_opening(host_kind, host, dormer) VALUES (?1, ?2, ?3)",
                params![kind, hid, (dormer && kind == "roof") as i64],
            )?;
            let id = db.last_insert_rowid();
            db.execute(
                "INSERT OR REPLACE INTO sketch(host_kind, host, data) VALUES ('host_opening', ?1, ?2)",
                params![id, text],
            )?;
            let inv = Inv::default()
                .sql(
                    "DELETE FROM sketch WHERE host_kind='host_opening' AND host=?1",
                    vec![id.into()],
                )
                .delete("host_opening", "id", id);
            Ok((id, id, inv))
        })
    }

    pub fn delete_host_opening(&mut self, id: i64) -> Result<(), Error> {
        self.op("host_opening_delete", |db| {
            let inv = Inv::default().restore(db, "host_opening", "id", id)?;
            if db.execute("DELETE FROM host_opening WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such opening {id}"));
            }
            let inv = crate::sketch::drop_owned(db, inv, "host_opening", id)?;
            Ok(((), id, inv))
        })
    }

    pub fn host_openings(&self, host: OpeningHost) -> Result<Vec<HostOpening>, Error> {
        let (k, id) = host.key();
        openings_of(&self.db, k, id)
    }

    /// Slab plan after shaft and opening cuts: pieces (outer ring counter-clockwise,
    /// holes clockwise); edge notches are part of the outer ring.
    pub fn slab_cut_pieces(
        &self,
        id: i64,
    ) -> Result<Vec<(Vec<(f64, f64)>, Vec<Vec<(f64, f64)>>)>, Error> {
        Ok(slab_cut(&self.db, id)?
            .map(|c| pieces_xy(&c))
            .unwrap_or_default())
    }

    /// Slab outlines for views: piece rings after cuts in sequence (even-odd).
    pub fn slab_view_rings(&self, id: i64) -> Result<Vec<Vec<(f64, f64)>>, Error> {
        Ok(slab_cut(&self.db, id)?
            .map(|c| c.rings().iter().map(|r| tup(r)).collect())
            .unwrap_or_default())
    }

    /// Applied slab cuts (shafts, openings): outlines for the symbol.
    pub fn slab_cut_rings(&self, id: i64) -> Result<Vec<Vec<(f64, f64)>>, Error> {
        Ok(slab_cut(&self.db, id)?
            .map(|c| c.applied.iter().map(|r| tup(r)).collect())
            .unwrap_or_default())
    }

    /// Ceiling outlines for views: piece rings after cuts in sequence (even-odd).
    pub fn ceiling_view_rings(&self, id: i64) -> Result<Vec<Vec<(f64, f64)>>, Error> {
        Ok(ceiling_cut(&self.db, id)?
            .map(|c| c.rings().iter().map(|r| tup(r)).collect())
            .unwrap_or_default())
    }

    /// Full-thickness ceiling body with shaft and opening cuts.
    pub fn ceiling_mesh(&self, id: i64) -> Result<ddc_geom::mesh::Mesh, Error> {
        let c = ceiling_cut(&self.db, id)?
            .ok_or_else(|| Error::Geom(format!("ceiling {id}: no boundary")))?;
        let (z0, z1) = crate::shafts::ceiling_z(&self.db, id)?;
        let tol = ddc_geom::Tol::default();
        let mut ms = vec![];
        for (o, hs) in &c.pieces {
            // a spike saved before they were refused still draws
            let p = ddc_geom::Profile::new_keeping_spikes(o.clone(), hs.clone(), tol)
                .map_err(|e| Error::Geom(format!("ceiling {id}: {e:?}")))?;
            ms.push(
                ddc_geom::Solid::extrude(&p, (z1 - z0).max(1e-3), tol)
                    .translated_z(z0)
                    .triangulate()
                    .map_err(|e| Error::Geom(format!("ceiling {id}: {e:?}")))?,
            );
        }
        Ok(concat(ms))
    }

    /// Warnings for cuts and attachments: a skipped opening/shaft (outside the host, breaks
    /// the plan), a slab shape without cuts, a wall whose top does not follow its source.
    pub fn cut_warnings(&self) -> Result<Vec<ElementWarning>, Error> {
        let db = &self.db;
        let mut out = vec![];
        let ids = |sql: &str| -> Result<Vec<i64>, Error> {
            let mut st = db.prepare(sql)?;
            let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            Ok(v)
        };
        let mut push = |c: Option<Cut>| {
            for (t, id, m) in c.into_iter().flat_map(|c| c.skipped) {
                out.push(ElementWarning {
                    table: t,
                    id,
                    message: m,
                });
            }
        };
        for id in ids("SELECT id FROM slab ORDER BY id")? {
            push(slab_cut(db, id).ok().flatten());
        }
        for id in ids("SELECT id FROM ceiling ORDER BY id")? {
            push(ceiling_cut(db, id).ok().flatten());
        }
        for id in ids("SELECT slab FROM slab_shape ORDER BY slab")? {
            if let Ok(Some((_, false))) = crate::sketch::shape_mesh_ex(db, id) {
                out.push(ElementWarning {
                    table: "slab",
                    id,
                    message: "shape with cuts not built: shown without shaft/opening cuts".into(),
                });
            }
        }
        out.extend(stale_walls(db)?);
        out.sort_by_key(|w| (w.table, w.id));
        out.dedup();
        Ok(out)
    }

    /// Attaches the wall top to a level (+ offset): the top follows the level elevation.
    pub fn attach_wall_to_level(
        &mut self,
        wall: i64,
        level: i64,
        offset: f64,
    ) -> Result<(), Error> {
        if !offset.is_finite() {
            return invalid("attach: offset".into());
        }
        crate::wall_spline::refuse_piece(&self.db, wall)?;
        self.op("wall_attach_level", |db| {
            if !exists(db, "wall", wall)? || !exists(db, "level", level)? {
                return invalid(format!("no such wall {wall} or level {level}"));
            }
            let mut inv = Inv::default()
                .delete("wall_attach_level", "wall", wall)
                .restore(db, "wall_attach_level", "wall", wall)?
                .restore(db, "wall", "id", wall)?;
            inv = inv
                .sql(
                    "DELETE FROM wall_attach WHERE wall=?1 AND side='top'",
                    vec![wall.into()],
                )
                .restore(db, "wall_attach", "wall", wall)?;
            db.execute(
                "DELETE FROM wall_attach WHERE wall=?1 AND side='top'",
                [wall],
            )?;
            db.execute("UPDATE wall SET top_roof=NULL, top_core=NULL, top_auto=0 WHERE id=?1", [wall])?;
            db.execute(
                "INSERT OR REPLACE INTO wall_attach_level(wall, level, offset) VALUES (?1, ?2, ?3)",
                params![wall, level, offset],
            )?;
            sync_wall(db, wall, true)?;
            Ok(((), wall, inv))
        })
    }

    /// Wall bottom (u, dz above the base), None is flat.
    pub fn wall_base_profile(&self, wall: i64) -> Result<Option<Vec<(f64, f64)>>, Error> {
        base_profile(&self.db, &mut Shapes::default(), wall)
    }

    /// Bottoms of `walls` in order; each slab's shape is built once.
    pub fn wall_base_profiles(&self, walls: &[i64]) -> Result<Vec<Option<Vec<(f64, f64)>>>, Error> {
        let mut sh = Shapes::default();
        walls
            .iter()
            .map(|&w| base_profile(&self.db, &mut sh, w))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CeilingSpec, LayerSpec, RoofKind, RoofSpec, ShaftSpec, SlabSpec, WallSpec};
    use ddc_geom::sketch::SketchCurve;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    }

    fn square_sketch(x0: f64, y0: f64, s: f64) -> Sketch {
        let p = [
            v2(x0, y0),
            v2(x0 + s, y0),
            v2(x0 + s, y0 + s),
            v2(x0, y0 + s),
        ];
        Sketch {
            curves: (0..4)
                .map(|i| SketchCurve::Line {
                    a: p[i],
                    b: p[(i + 1) % 4],
                })
                .collect(),
        }
    }

    fn base() -> (Model, i64, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l1 = m.add_level("L1", 0.0).unwrap();
        let l2 = m.add_level("L2", 3.0).unwrap();
        let st = m
            .add_slab_type(
                "floor",
                &[LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        (m, l1, l2, st)
    }

    #[test]
    fn sketch_opening_cuts_slab_mesh_quantities_and_view_rings_one_undo() {
        let (mut m, l1, _, st) = base();
        let s = m
            .add_slab(&SlabSpec {
                level: l1,
                slab_type: st,
                offset: 0.0,
                outer: rect(0.0, 0.0, 6.0, 4.0),
                holes: vec![],
            })
            .unwrap();
        let v0 = m.slab_mesh(s).unwrap().volume();
        let o = m
            .add_host_opening(OpeningHost::Slab(s), &square_sketch(1.0, 1.0, 1.0), false)
            .unwrap();
        let q = m.slab_quantities(s).unwrap();
        assert!((q[0].area - 23.0).abs() < 1e-9, "{}", q[0].area);
        assert!((m.slab_mesh(s).unwrap().volume() - (v0 - 0.2)).abs() < 1e-9);
        assert_eq!(m.slab_view_rings(s).unwrap().len(), 2);
        assert_eq!(m.host_openings(OpeningHost::Slab(s)).unwrap()[0].id, o);
        m.undo().unwrap();
        assert!((m.slab_quantities(s).unwrap()[0].area - 24.0).abs() < 1e-9);
        m.add_host_opening(OpeningHost::Slab(s), &square_sketch(1.0, 1.0, 1.0), false)
            .unwrap();
        // deleting the slab removes the opening and sketch; undo restores them
        m.delete_slab(s).unwrap();
        assert!(m.check_integrity().unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.host_openings(OpeningHost::Slab(s)).unwrap().len(), 1);
    }

    #[test]
    fn shaft_through_floors_cuts_every_slab_mesh() {
        let (mut m, l1, l2, st) = base();
        let mut ids = vec![];
        for l in [l1, l2] {
            ids.push(
                m.add_slab(&SlabSpec {
                    level: l,
                    slab_type: st,
                    offset: 0.0,
                    outer: rect(0.0, 0.0, 6.0, 4.0),
                    holes: vec![],
                })
                .unwrap(),
            );
        }
        m.add_shaft(&ShaftSpec {
            level: l1,
            top_level: l2,
            base_offset: -0.5,
            top_offset: 0.5,
            ring: rect(2.0, 1.0, 3.0, 2.0),
        })
        .unwrap();
        for s in ids {
            let v = m.slab_mesh(s).unwrap().volume();
            assert!((v - 23.0 * 0.2).abs() < 1e-9, "slab {s}: {v}");
            assert_eq!(m.slab_view_rings(s).unwrap().len(), 2);
        }
    }

    #[test]
    fn ceiling_opening_deducts_quantities() {
        let (mut m, l1, _, st) = base();
        let c = m
            .add_ceiling(&CeilingSpec {
                level: l1,
                slab_type: st,
                height: 2.7,
                outer: rect(0.0, 0.0, 4.0, 4.0),
                holes: vec![],
            })
            .unwrap();
        m.add_host_opening(
            OpeningHost::Ceiling(c),
            &square_sketch(1.0, 1.0, 0.5),
            false,
        )
        .unwrap();
        let q = m.ceiling_quantities(c).unwrap();
        assert!((q[0].1 - 15.75).abs() < 1e-9, "{q:?}");
        assert_eq!(m.ceiling_view_rings(c).unwrap().len(), 2);
    }

    #[test]
    fn dormer_opening_cuts_roof_area_and_mesh() {
        let (mut m, l1, _, st) = base();
        let r = m
            .add_roof(&RoofSpec {
                level: l1,
                slab_type: st,
                offset: 3.0,
                overhang: 0.0,
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: rect(0.0, 0.0, 10.0, 6.0),
                    slopes: vec![Some(30.0), None, Some(30.0), None],
                    mansard: None,
                },
            })
            .unwrap();
        let a0 = m.roof_quantities(r).unwrap().layers[0].2;
        m.add_host_opening(OpeningHost::Roof(r), &square_sketch(4.0, 0.5, 1.0), true)
            .unwrap();
        let a1 = m.roof_quantities(r).unwrap().layers[0].2;
        let k = 1.0 / 30f64.to_radians().cos();
        assert!((a0 - a1 - k).abs() < 1e-6, "{a0} {a1}");
        assert!(m.roof_mesh(r).unwrap().volume() > 0.0);
        m.undo().unwrap();
        assert!((m.roof_quantities(r).unwrap().layers[0].2 - a0).abs() < 1e-9);
    }

    #[test]
    fn wall_attached_to_sloped_slab_follows_it_and_level_attach() {
        let (mut m, l1, l2, st) = base();
        let wt = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0.0, 1.0),
                b: (4.0, 1.0),
                level: l1,
                base_offset: 0.0,
                height: 2.5,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        let s = m
            .add_slab(&SlabSpec {
                level: l2,
                slab_type: st,
                offset: 0.0,
                outer: rect(-1.0, -1.0, 5.0, 3.0),
                holes: vec![],
            })
            .unwrap();
        m.attach_wall_to_slab(w, crate::WallSide::Top, s).unwrap();
        let top = m.wall_top_profile(w).unwrap().unwrap();
        assert!(top.iter().all(|p| (p.1 - 2.8).abs() < 1e-9), "{top:?}");
        // slab slope: the wall top follows in the same operation
        m.set_slab_slope(
            s,
            Some(ddc_geom::slope::Plane {
                p0: v2(0.0, 1.0),
                gx: 0.1,
                gy: 0.0,
            }),
        )
        .unwrap();
        let top = m.wall_top_profile(w).unwrap().unwrap();
        assert!(
            (ddc_geom::top::height_at(&top, 4.0) - 3.2).abs() < 1e-9,
            "{top:?}"
        );
        let v: f64 = m
            .sql()
            .query_row("SELECT SUM(volume) FROM wall_qty WHERE wall=?1", [w], |r| {
                r.get(0)
            })
            .unwrap();
        assert!((v - 0.2 * 4.0 * 3.0).abs() < 1e-6, "{v}");
        m.undo().unwrap();
        assert!(
            (ddc_geom::top::height_at(&m.wall_top_profile(w).unwrap().unwrap(), 4.0) - 2.8).abs()
                < 1e-9
        );
        m.attach_wall_to_level(w, l2, -0.5).unwrap();
        let top = m.wall_top_profile(w).unwrap().unwrap();
        assert!(top.iter().all(|p| (p.1 - 2.5).abs() < 1e-9));
        m.update_level(l2, "L2", 4.0).unwrap();
        let top = m.wall_top_profile(w).unwrap().unwrap();
        assert!(top.iter().all(|p| (p.1 - 3.5).abs() < 1e-9), "{top:?}");
    }

    fn slab(m: &mut Model, l: i64, st: i64) -> i64 {
        m.add_slab(&SlabSpec {
            level: l,
            slab_type: st,
            offset: 0.0,
            outer: rect(0.0, 0.0, 6.0, 4.0),
            holes: vec![],
        })
        .unwrap()
    }

    #[test]
    fn deleting_wall_opening_keeps_slab_opening_with_same_id() {
        let (mut m, l1, _, st) = base();
        let wt = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                level: l1,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        let s = slab(&mut m, l1, st);
        let o = m
            .add_host_opening(OpeningHost::Slab(s), &square_sketch(1.0, 1.0, 1.0), false)
            .unwrap();
        // a wall opening (door) with the same id as a slab opening
        let d = m
            .add_opening(
                w,
                &crate::OpeningSpec {
                    along: 1.0,
                    width: 0.9,
                    sill: 0.0,
                    height: 2.1,
                    kind: "door".into(),
                },
            )
            .unwrap();
        assert_eq!(d, o);
        m.delete_opening(o).unwrap();
        assert_eq!(m.host_openings(OpeningHost::Slab(s)).unwrap().len(), 1);
        assert!(m.check_integrity().unwrap().is_empty());
    }

    #[test]
    fn legacy_opening_sketch_kind_is_migrated() {
        let (mut m, l1, _, st) = base();
        let s = slab(&mut m, l1, st);
        let o = m
            .add_host_opening(OpeningHost::Slab(s), &square_sketch(1.0, 1.0, 1.0), false)
            .unwrap();
        m.sql()
            .execute(
                "UPDATE sketch SET host_kind='opening' WHERE host_kind='host_opening'",
                [],
            )
            .unwrap();
        migrate_sketch_kind(m.sql()).unwrap();
        assert_eq!(m.host_openings(OpeningHost::Slab(s)).unwrap()[0].id, o);
    }

    #[test]
    fn opening_outside_host_is_rejected() {
        let (mut m, l1, _, st) = base();
        let s = slab(&mut m, l1, st);
        let e = m
            .add_host_opening(OpeningHost::Slab(s), &square_sketch(5.5, 1.0, 1.0), false)
            .unwrap_err();
        assert!(
            matches!(e, Error::OpeningOutsideHost { kind: "slab", .. }),
            "{e}"
        );
        let e = m
            .add_host_opening(OpeningHost::Slab(s), &square_sketch(10.0, 1.0, 1.0), false)
            .unwrap_err();
        assert!(matches!(e, Error::OpeningOutsideHost { .. }));
    }

    #[test]
    fn edge_and_overlapping_cuts_match_in_mesh_and_quantities() {
        let (mut m, l1, l2, st) = base();
        let s = slab(&mut m, l1, st);
        // an opening at the edge is a notch; the second overlaps the first
        m.add_host_opening(OpeningHost::Slab(s), &square_sketch(5.0, 1.0, 1.0), false)
            .unwrap();
        m.add_host_opening(OpeningHost::Slab(s), &square_sketch(4.5, 1.5, 1.0), false)
            .unwrap();
        // a shaft across the slab edge
        m.add_shaft(&ShaftSpec {
            level: l1,
            top_level: l2,
            base_offset: -0.5,
            top_offset: 0.5,
            ring: rect(-0.5, 3.0, 1.0, 5.0),
        })
        .unwrap();
        let cut = 1.0 + 1.0 - 0.25 + 1.0;
        let q = m.slab_quantities(s).unwrap();
        assert!((q[0].area - (24.0 - cut)).abs() < 1e-9, "{}", q[0].area);
        let v = m.slab_mesh(s).unwrap().volume();
        assert!((v - (24.0 - cut) * 0.2).abs() < 1e-9, "{v}");
        let p = m.slab_cut_pieces(s).unwrap();
        assert_eq!((p.len(), p[0].1.len()), (1, 0));
        assert!(m.cut_warnings().unwrap().is_empty());
    }

    #[test]
    fn cut_across_slab_gives_two_pieces() {
        let (mut m, l1, l2, st) = base();
        let s = slab(&mut m, l1, st);
        m.add_shaft(&ShaftSpec {
            level: l1,
            top_level: l2,
            base_offset: -0.5,
            top_offset: 0.5,
            ring: rect(2.0, -1.0, 3.0, 5.0),
        })
        .unwrap();
        assert_eq!(m.slab_cut_pieces(s).unwrap().len(), 2);
        let v = m.slab_mesh(s).unwrap().volume();
        assert!((v - 20.0 * 0.2).abs() < 1e-9, "{v}");
        assert!((m.slab_quantities(s).unwrap()[0].area - 20.0).abs() < 1e-9);
    }

    #[test]
    fn opening_over_shaft_is_not_deducted_twice() {
        let (mut m, l1, l2, st) = base();
        let c = m
            .add_ceiling(&CeilingSpec {
                level: l1,
                slab_type: st,
                height: 2.7,
                outer: rect(0.0, 0.0, 4.0, 4.0),
                holes: vec![],
            })
            .unwrap();
        m.add_shaft(&ShaftSpec {
            level: l1,
            top_level: l2,
            base_offset: 0.0,
            top_offset: 0.0,
            ring: rect(1.0, 1.0, 2.0, 2.0),
        })
        .unwrap();
        m.add_host_opening(
            OpeningHost::Ceiling(c),
            &square_sketch(1.0, 1.0, 1.0),
            false,
        )
        .unwrap();
        let q = m.ceiling_quantities(c).unwrap();
        assert!((q[0].1 - 15.0).abs() < 1e-9, "{q:?}");
    }

    #[test]
    fn roof_opening_cuts_flat_deck_dormer_only_slopes() {
        let (mut m, l1, _, st) = base();
        let r = m
            .add_roof(&RoofSpec {
                level: l1,
                slab_type: st,
                offset: 3.0,
                overhang: 0.0,
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: rect(0.0, 0.0, 10.0, 6.0),
                    slopes: vec![Some(30.0); 4],
                    mansard: Some((1.0, None)),
                },
            })
            .unwrap();
        let a0 = m.roof_quantities(r).unwrap().layers[0].2;
        // in the centre: the horizontal platform of the mansard
        let o = m
            .add_host_opening(OpeningHost::Roof(r), &square_sketch(4.5, 2.5, 1.0), true)
            .unwrap();
        let a1 = m.roof_quantities(r).unwrap().layers[0].2;
        m.delete_host_opening(o).unwrap();
        m.add_host_opening(OpeningHost::Roof(r), &square_sketch(4.5, 2.5, 1.0), false)
            .unwrap();
        let a2 = m.roof_quantities(r).unwrap().layers[0].2;
        assert!(a1 >= a0 - 1e-9, "dormer must not cut the deck: {a0} {a1}");
        assert!(a2 < a0 - 1e-3, "opening cuts the deck: {a0} {a2}");
    }

    #[test]
    fn base_attach_keeps_roof_top() {
        let (mut m, l1, _, st) = base();
        let wt = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (1.0, 3.0),
                b: (9.0, 3.0),
                level: l1,
                base_offset: 0.0,
                height: 2.5,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        let r = m
            .add_roof(&RoofSpec {
                level: l1,
                slab_type: st,
                offset: 3.0,
                overhang: 0.0,
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: rect(0.0, 0.0, 10.0, 6.0),
                    slopes: vec![Some(30.0), None, Some(30.0), None],
                    mansard: None,
                },
            })
            .unwrap();
        m.attach_wall_top(w, r).unwrap();
        let top = m.wall_top_profile(w).unwrap();
        assert!(top.is_some());
        let s = m
            .add_slab(&SlabSpec {
                level: l1,
                slab_type: st,
                offset: 0.0,
                outer: rect(0.0, 0.0, 10.0, 6.0),
                holes: vec![],
            })
            .unwrap();
        m.attach_wall_to_slab(w, crate::WallSide::Base, s).unwrap();
        assert_eq!(m.wall_top_profile(w).unwrap(), top);
    }

    #[test]
    fn envelope_of_gable_and_stepped_profiles() {
        let gable = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (2.0, 4.0), (0.0, 3.0)];
        assert_eq!(
            envelope(&gable, true),
            vec![(0.0, 3.0), (2.0, 4.0), (4.0, 3.0)]
        );
        assert_eq!(envelope(&gable, false), vec![(0.0, 0.0), (4.0, 0.0)]);
        let step = [
            (0.0, 0.0),
            (4.0, 0.0),
            (4.0, 2.0),
            (2.0, 2.0),
            (2.0, 3.0),
            (0.0, 3.0),
        ];
        assert_eq!(
            envelope(&step, true),
            vec![(0.0, 3.0), (2.0, 3.0), (2.0, 2.0), (4.0, 2.0)]
        );
    }
}
