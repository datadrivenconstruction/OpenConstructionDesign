// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Spline walls (geometry plan W5). The exact planar NURBS axis lives in `wall_spline`; the
//! wall itself is a chain of ordinary straight or arc wall rows (`wall_spline_piece`) fitted
//! to the axis by tangent-continuous biarcs within the wall's tolerance. The pieces carry the
//! joins at the two outer ends, the openings, the 3D bodies and the room boundaries like any
//! wall; their inner ends are set not to join, so neighbours meet on their common normal and
//! the layers run on without a seam. Quantities of the whole wall are taken exactly from the
//! spline (layer areas by Gauss), the chain's join and opening effects kept.

use ddc_geom::nurbs::Curve;
use ddc_geom::wall_spline::{ArcPiece, SplineAxis, SplineError};
use ddc_geom::{v2, v3, V2};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS wall_spline(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, wtype INTEGER NOT NULL,
    axis TEXT NOT NULL, base_offset REAL NOT NULL, height REAL NOT NULL, offset0 REAL, tol REAL NOT NULL);
CREATE TABLE IF NOT EXISTS wall_spline_piece(wall INTEGER PRIMARY KEY, spline INTEGER NOT NULL, pos INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS wall_spline_piece_spline ON wall_spline_piece(spline, pos);
";

/// Default fitting tolerance of the arc chain, m.
pub const SPLINE_TOL: f64 = 1e-3;
/// Tolerance range accepted, m.
pub const SPLINE_TOL_RANGE: (f64, f64) = (1e-5, 0.05);
/// Most pieces one spline wall may take.
pub const MAX_PIECES: usize = 512;

/// A wall on a spline axis.
#[derive(Clone, Debug, PartialEq)]
pub struct SplineWallSpec {
    pub level: i64,
    pub wall_type: i64,
    /// Planar axis, z = 0 (plan coordinates).
    pub axis: Curve,
    pub base_offset: f64,
    pub height: f64,
    /// Offset of the first layer to the left of the axis; None: centred.
    pub offset0: Option<f64>,
    /// Largest distance of the arc chain from the axis, m.
    pub tol: f64,
}

impl SplineWallSpec {
    /// Wall on a cubic through plan points.
    pub fn through(level: i64, wall_type: i64, pts: &[(f64, f64)], height: f64) -> Result<SplineWallSpec, Error> {
        let p: Vec<V2> = pts.iter().map(|q| v2(q.0, q.1)).collect();
        let axis = SplineAxis::through(&p).map_err(spline_err)?;
        Ok(SplineWallSpec {
            level,
            wall_type,
            axis: axis.curve().clone(),
            base_offset: 0.0,
            height,
            offset0: None,
            tol: SPLINE_TOL,
        })
    }
}

/// Exact quantities of one layer of a spline wall.
#[derive(Clone, Debug, PartialEq)]
pub struct SplineLayerQty {
    pub material: String,
    /// Plan area: the exact spline layer area plus the joins' effect at the outer ends.
    pub plan_area: f64,
    pub volume: f64,
}

fn spline_err(e: SplineError) -> Error {
    Error::Invalid(match e {
        SplineError::NotPlanar => "spline wall: the axis must be a plan curve (z = 0)".into(),
        SplineError::Degenerate => "spline wall: the axis has no length or a cusp".into(),
        SplineError::Folds { offset, radius } => format!(
            "spline wall: a face at {offset:.3} m folds where the axis bends to a radius of {radius:.3} m"
        ),
        SplineError::Refit { deviation } => {
            format!("spline wall: the axis cannot be followed within the tolerance ({deviation:.2e} m)")
        }
    })
}

fn axis_json(c: &Curve) -> String {
    serde_json::json!({
        "degree": c.degree(),
        "knots": c.knots(),
        "ctrl": c.ctrl().iter().map(|p| [p.x, p.y]).collect::<Vec<_>>(),
        "weights": c.weights(),
    })
    .to_string()
}

fn axis_of(s: &str) -> Result<Curve, Error> {
    let bad = || Error::Invalid("spline wall: unreadable axis".into());
    let v: serde_json::Value = serde_json::from_str(s).map_err(|_| bad())?;
    let f = |k: &str| -> Option<Vec<f64>> { v[k].as_array()?.iter().map(|x| x.as_f64()).collect() };
    let ctrl: Vec<_> = v["ctrl"]
        .as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|p| Some(v3(p[0].as_f64()?, p[1].as_f64()?, 0.0)))
        .collect::<Option<_>>()
        .ok_or_else(bad)?;
    let degree = v["degree"].as_u64().ok_or_else(bad)? as usize;
    Curve::new(degree, f("knots").ok_or_else(bad)?, ctrl, f("weights").ok_or_else(bad)?).map_err(|_| bad())
}

/// Layer thicknesses of a wall type, outside in.
fn layers(db: &Connection, wtype: i64) -> Result<Vec<(String, f64)>, Error> {
    Ok(db
        .prepare_cached("SELECT material, thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos")?
        .query_map([wtype], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?)
}

/// Left offsets of the layer faces as the wall strip lays them: [o0, o0 + t1, ...] with
/// o0 = offset0, or -total / 2 for a centred wall.
fn faces(db: &Connection, s: &SplineWallSpec) -> Result<Vec<f64>, Error> {
    let ls = layers(db, s.wall_type)?;
    let total: f64 = ls.iter().map(|l| l.1).sum();
    let mut o = s.offset0.unwrap_or(-total / 2.0);
    let mut out = vec![o];
    for l in &ls {
        o += l.1;
        out.push(o);
    }
    Ok(out)
}

fn check(db: &Connection, s: &SplineWallSpec) -> Result<SplineAxis, Error> {
    let ok = s.height.is_finite()
        && s.height > 0.0
        && s.base_offset.is_finite()
        && s.offset0.is_none_or(f64::is_finite)
        && (SPLINE_TOL_RANGE.0..=SPLINE_TOL_RANGE.1).contains(&s.tol)
        && crate::walls::exists(db, "level", s.level)?
        && crate::walls::exists(db, "wall_type", s.wall_type)?;
    if !ok {
        return Err(Error::Invalid(format!(
            "spline wall: bad height, level, type or tolerance (tolerance {}..{} m)",
            SPLINE_TOL_RANGE.0, SPLINE_TOL_RANGE.1
        )));
    }
    let ax = SplineAxis::new(s.axis.clone()).map_err(spline_err)?;
    for d in faces(db, s)? {
        ax.check_offset(d).map_err(spline_err)?;
    }
    Ok(ax)
}

/// The arc chain of the axis; every piece a valid wall axis.
fn fit(ax: &SplineAxis, tol: f64) -> Result<Vec<ArcPiece>, Error> {
    let ps = ax.biarcs(tol).map_err(spline_err)?;
    if ps.len() > MAX_PIECES {
        return Err(Error::Invalid(format!(
            "spline wall: {} pieces at this tolerance (at most {MAX_PIECES}); loosen it or smooth the axis",
            ps.len()
        )));
    }
    for (a, b, bulge) in &ps {
        if (*b - *a).len() < crate::walls::MIN_LEN {
            return Err(Error::Invalid(
                "spline wall: the axis bends too sharply for the tolerance (a piece under 1 mm)".into(),
            ));
        }
        crate::walls::check_bulge((a.x, a.y), (b.x, b.y), *bulge)?;
    }
    Ok(ps)
}

fn piece_len((a, b, bulge): &ArcPiece) -> f64 {
    ddc_geom::arc::axis_length(*a, *b, *bulge)
}

/// Wall ids of the pieces in order.
fn piece_ids(db: &Connection, id: i64) -> Result<Vec<i64>, Error> {
    Ok(db
        .prepare_cached("SELECT wall FROM wall_spline_piece WHERE spline=?1 ORDER BY pos")?
        .query_map([id], |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}

fn read(db: &Connection, id: i64) -> Result<SplineWallSpec, Error> {
    let row: Option<(i64, i64, String, f64, f64, Option<f64>, f64)> = db
        .query_row(
            "SELECT level, wtype, axis, base_offset, height, offset0, tol FROM wall_spline WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .optional()?;
    let (level, wall_type, axis, base_offset, height, offset0, tol) =
        row.ok_or_else(|| Error::Invalid(format!("no such spline wall {id}")))?;
    Ok(SplineWallSpec { level, wall_type, axis: axis_of(&axis)?, base_offset, height, offset0, tol })
}

/// Openings of the pieces as (opening id, distance along the whole axis, width).
fn openings(db: &Connection, pieces: &[i64]) -> Result<Vec<(i64, f64, f64)>, Error> {
    let mut out = vec![];
    let mut start = 0.0;
    for &w in pieces {
        let (ax, ay, bx, by, bulge): (f64, f64, f64, f64, f64) = db.query_row(
            "SELECT ax, ay, bx, by, bulge FROM wall WHERE id=?1",
            [w],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        let mut st = db.prepare_cached("SELECT id, along, width FROM opening WHERE wall=?1")?;
        for o in st.query_map([w], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?)))? {
            let (id, along, width) = o?;
            out.push((id, start + along, width));
        }
        start += ddc_geom::arc::axis_length(v2(ax, ay), v2(bx, by), bulge);
    }
    Ok(out)
}

/// Writes the pieces of `s` for spline `id`: reuses `old` wall rows in order, inserts the
/// rest, deletes the surplus; inner ends set not to join; openings re-hosted by their
/// distance along the axis. With `scale`, the old rows are a chain standing for the axis:
/// each opening's centre moves by the length parameter (distance times `scale`) and the
/// opening slides into the piece under its centre. Returns the inverse.
fn write_pieces(
    db: &Connection,
    id: i64,
    s: &SplineWallSpec,
    ps: &[ArcPiece],
    old: &[i64],
    scale: Option<f64>,
) -> Result<Inv, Error> {
    crate::wall_edit::ensure(db)?;
    let ops = openings(db, old)?;
    let mut inv = Inv::default();
    for &w in old {
        // undo runs in order: the rewritten join and chain rows go, then the old rows return
        inv = inv
            .delete("wall_join", "wall", w)
            .delete("wall_spline_piece", "wall", w)
            .restore(db, "wall", "id", w)?
            .restore(db, "wall_join", "wall", w)?
            .restore(db, "opening", "wall", w)?
            .restore(db, "wall_spline_piece", "wall", w)?;
    }
    let mut ids = vec![];
    for (k, (a, b, bulge)) in ps.iter().enumerate() {
        let kind = (*bulge != 0.0) as i64;
        let w = match old.get(k) {
            Some(&w) => {
                db.execute(
                    "UPDATE wall SET level=?1, wtype=?2, ax=?3, ay=?4, bx=?5, by=?6, base_offset=?7, height=?8, \
                     offset0=?9, axis_kind=?10, bulge=?11, top_profile=NULL, lean=0, top_scale=1 WHERE id=?12",
                    params![s.level, s.wall_type, a.x, a.y, b.x, b.y, s.base_offset, s.height, s.offset0, kind, bulge, w],
                )?;
                db.execute("DELETE FROM wall_join WHERE wall=?1", [w])?;
                db.execute("DELETE FROM wall_spline_piece WHERE wall=?1", [w])?;
                w
            }
            None => {
                db.execute(
                    "INSERT INTO wall(level,wtype,ax,ay,bx,by,base_offset,height,offset0,axis_kind,bulge) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![s.level, s.wall_type, a.x, a.y, b.x, b.y, s.base_offset, s.height, s.offset0, kind, bulge],
                )?;
                let w = db.last_insert_rowid();
                inv = inv.delete("wall", "id", w).delete("wall_join", "wall", w).delete("wall_spline_piece", "wall", w);
                w
            }
        };
        db.execute("INSERT INTO wall_spline_piece(wall, spline, pos) VALUES (?1, ?2, ?3)", params![w, id, k as i64])?;
        for (end, inner) in [(0i64, k > 0), (1i64, k + 1 < ps.len())] {
            if inner {
                db.execute(
                    "INSERT INTO wall_join(wall, end_, kind, allowed) VALUES (?1, ?2, 'butt', 0)",
                    params![w, end],
                )?;
            }
        }
        ids.push(w);
    }
    // openings: each must fit inside one piece of the new chain
    let lens: Vec<f64> = ps.iter().map(piece_len).collect();
    for (oid, along, width) in ops {
        let mut start = 0.0;
        let mut host = None;
        for (k, l) in lens.iter().enumerate() {
            match scale {
                None => {
                    if along >= start - 1e-9 && along + width <= start + l + 1e-9 {
                        host = Some((ids[k], (along - start).max(0.0)));
                        break;
                    }
                }
                Some(f) => {
                    let mid = (along + width / 2.0) * f;
                    let last = k + 1 == lens.len();
                    if mid <= start + l || last {
                        if *l + 1e-9 >= width {
                            host = Some((ids[k], (mid - width / 2.0 - start).min(l - width).max(0.0)));
                        }
                        break;
                    }
                }
            }
            start += l;
        }
        let Some((w, a)) = host else {
            return Err(Error::Invalid(format!(
                "spline wall {id}: opening {oid} would cross a joint of the arc chain; move it or change the tolerance"
            )));
        };
        db.execute("UPDATE opening SET wall=?1, along=?2 WHERE id=?3", params![w, a, oid])?;
    }
    inv = inv.then(crate::footings::follow_pieces(db, old, &ids)?);
    for &w in old.iter().skip(ps.len()) {
        inv = crate::walls::drop_wall_row(db, inv, w)?;
    }
    if old.len() > ps.len() {
        inv = crate::roof_edge::drop_hosts(db, inv, "wall", &old[ps.len()..])?;
    }
    Ok(inv)
}

/// The spline wall a wall row is a piece of.
fn piece_of(db: &Connection, wall: i64) -> Result<Option<i64>, Error> {
    if !crate::mesh_edit::has_table(db, "wall_spline_piece")? {
        return Ok(None);
    }
    Ok(db
        .query_row("SELECT spline FROM wall_spline_piece WHERE wall=?1", [wall], |r| r.get(0))
        .optional()?)
}

fn piece_error(wall: i64, spline: i64) -> Error {
    Error::Invalid(format!("wall {wall} is a piece of spline wall {spline}: edit the spline wall as a whole"))
}

/// A direct edit of one piece is refused: the chain follows its exact axis only.
pub(crate) fn refuse_piece(db: &Connection, wall: i64) -> Result<(), Error> {
    match piece_of(db, wall)? {
        Some(s) => Err(piece_error(wall, s)),
        None => Ok(()),
    }
}

/// The inner ends of the chain stay unjoined; its two outer ends join like any wall end.
pub(crate) fn refuse_inner_end(db: &Connection, wall: i64, end: u8) -> Result<(), Error> {
    let Some(s) = piece_of(db, wall)? else { return Ok(()) };
    let ps = piece_ids(db, s)?;
    let k = ps.iter().position(|&p| p == wall).unwrap_or(0);
    if (end == 0 && k > 0) || (end == 1 && k + 1 < ps.len()) {
        return Err(Error::Invalid(format!(
            "wall {wall}: end {end} is inside spline wall {s}; only its two outer ends join"
        )));
    }
    Ok(())
}

/// A bulk write of a piece row passes only when it leaves the row as it is (tables that list
/// every wall row write the pieces back on a round trip).
pub(crate) fn refuse_piece_change(db: &Connection, id: i64, w: &crate::WallSpec, bulge: f64) -> Result<(), Error> {
    let Some(s) = piece_of(db, id)? else { return Ok(()) };
    let same: bool = db.query_row(
        "SELECT COALESCE(level=?2 AND wtype=?3 AND abs(ax-?4)<1e-9 AND abs(ay-?5)<1e-9 AND abs(bx-?6)<1e-9 \
         AND abs(by-?7)<1e-9 AND abs(base_offset-?8)<1e-9 AND abs(height-?9)<1e-9 AND abs(bulge-?10)<1e-12 \
         AND (offset0 IS ?11 OR abs(offset0-?11)<1e-9), 0) FROM wall WHERE id=?1",
        params![id, w.level, w.wall_type, w.a.0, w.a.1, w.b.0, w.b.1, w.base_offset, w.height, bulge, w.offset0],
        |r| r.get(0),
    )?;
    if same {
        Ok(())
    } else {
        Err(piece_error(id, s))
    }
}

/// Same for a bulk write of the lean of a piece.
pub(crate) fn refuse_piece_shape(db: &Connection, id: i64, lean: f64, top_scale: f64) -> Result<(), Error> {
    let Some(s) = piece_of(db, id)? else { return Ok(()) };
    let same: bool = db.query_row(
        "SELECT COALESCE(abs(lean-?2)<1e-12 AND abs(top_scale-?3)<1e-12, 0) FROM wall WHERE id=?1",
        params![id, lean, top_scale],
        |r| r.get(0),
    )?;
    if same {
        Ok(())
    } else {
        Err(piece_error(id, s))
    }
}

/// Spline walls with pieces among `walls`, each with its pieces in axis order. A spline wall
/// only partly among them is refused: its pieces move, turn or copy together.
pub(crate) fn whole_groups(db: &Connection, walls: &[i64]) -> Result<Vec<(i64, Vec<i64>)>, Error> {
    let mut out: Vec<(i64, Vec<i64>)> = vec![];
    for &w in walls {
        let Some(s) = piece_of(db, w)? else { continue };
        if out.iter().any(|g| g.0 == s) {
            continue;
        }
        let ps = piece_ids(db, s)?;
        if let Some(&miss) = ps.iter().find(|p| !walls.contains(p)) {
            return Err(Error::Invalid(format!(
                "wall {miss} of spline wall {s} is not selected: a spline wall moves, turns or copies as a whole"
            )));
        }
        out.push((s, ps));
    }
    Ok(out)
}

/// Clipboard: a piece or a spline wall stands for the spline wall and all its pieces.
pub(crate) fn group_items(db: &Connection, kind: &str, id: i64) -> Result<Vec<(&'static str, i64)>, Error> {
    let s = match kind {
        "wall" => piece_of(db, id)?,
        "wall_spline" => Some(id),
        _ => None,
    };
    let Some(s) = s else { return Ok(vec![]) };
    let mut out = vec![("wall_spline", s)];
    out.extend(piece_ids(db, s)?.into_iter().map(|p| ("wall", p)));
    Ok(out)
}

/// The axis under the plan map p -> m p + t (control points map exactly under an affine map).
/// A mirror also reverses it: mirrored wall rows swap their ends to keep the layers' side.
fn mapped_axis(c: &Curve, m: [[f64; 2]; 2], t: (f64, f64)) -> Result<Curve, Error> {
    let mut ctrl: Vec<_> =
        c.ctrl().iter().map(|p| v3(m[0][0] * p.x + m[0][1] * p.y + t.0, m[1][0] * p.x + m[1][1] * p.y + t.1, 0.0)).collect();
    let mut knots = c.knots().to_vec();
    let mut weights = c.weights().to_vec();
    if m[0][0] * m[1][1] - m[0][1] * m[1][0] < 0.0 {
        let (lo, hi) = (knots[0], knots[knots.len() - 1]);
        knots = knots.iter().rev().map(|k| lo + hi - k).collect();
        ctrl.reverse();
        weights.reverse();
    }
    Curve::new(c.degree(), knots, ctrl, weights).map_err(|_| Error::Invalid("spline wall: bad mapped axis".into()))
}

/// After a plan transform has placed the `pieces` (moved rows, or copies when `copy`) of
/// spline wall `id`: the axis follows the map and the chain is refitted on it. Returns the
/// spline wall written, its pieces and, for a move, the inverse of the rewrite (a copy's rows
/// are new to the batch, its caller watermarks the tables).
pub(crate) fn follow_xf(
    db: &Connection,
    id: i64,
    pieces: &[i64],
    m: [[f64; 2]; 2],
    t: (f64, f64),
    copy: bool,
) -> Result<(i64, Vec<i64>, Inv), Error> {
    let mut s = read(db, id)?;
    s.axis = mapped_axis(&s.axis, m, t)?;
    let mut rows = pieces.to_vec();
    if m[0][0] * m[1][1] - m[0][1] * m[1][0] < 0.0 {
        rows.reverse();
    }
    let ps = fit(&check(db, &s)?, s.tol)?;
    let (sid, inv) = if copy {
        db.execute(
            "INSERT INTO wall_spline(level, wtype, axis, base_offset, height, offset0, tol) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![s.level, s.wall_type, axis_json(&s.axis), s.base_offset, s.height, s.offset0, s.tol],
        )?;
        let sid = db.last_insert_rowid();
        write_pieces(db, sid, &s, &ps, &rows, None)?;
        (sid, Inv::default())
    } else {
        let inv = Inv::default().restore(db, "wall_spline", "id", id)?;
        db.execute("UPDATE wall_spline SET axis=?2 WHERE id=?1", params![id, axis_json(&s.axis)])?;
        (id, inv.then(write_pieces(db, id, &s, &ps, &rows, None)?))
    };
    Ok((sid, piece_ids(db, sid)?, inv))
}

/// Clipboard: the axis shifted by (dx, dy).
pub(crate) fn translate_axis(text: &str, dx: f64, dy: f64) -> Option<String> {
    let c = axis_of(text).ok()?;
    Some(axis_json(&mapped_axis(&c, [[1.0, 0.0], [0.0, 1.0]], (dx, dy)).ok()?))
}

impl Model {
    /// Adds a wall on a spline axis; returns the spline wall id. One undo step.
    pub fn add_spline_wall(&mut self, s: &SplineWallSpec) -> Result<i64, Error> {
        let ax = check(&self.db, s)?;
        let ps = fit(&ax, s.tol)?;
        self.op("wall_spline_add", |db| {
            db.execute(
                "INSERT INTO wall_spline(level, wtype, axis, base_offset, height, offset0, tol) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![s.level, s.wall_type, axis_json(&s.axis), s.base_offset, s.height, s.offset0, s.tol],
            )?;
            let id = db.last_insert_rowid();
            let inv = write_pieces(db, id, s, &ps, &[], None)?;
            Ok((id, id, inv.delete("wall_spline", "id", id)))
        })
    }

    /// Replaces the axis, type, heights or tolerance; the pieces follow, their openings kept
    /// at the same distance along the axis. One undo step.
    pub fn update_spline_wall(&mut self, id: i64, s: &SplineWallSpec) -> Result<(), Error> {
        let ax = check(&self.db, s)?;
        let ps = fit(&ax, s.tol)?;
        self.op("wall_spline_edit", |db| {
            read(db, id)?;
            let old = piece_ids(db, id)?;
            let inv = Inv::default().restore(db, "wall_spline", "id", id)?;
            db.execute(
                "UPDATE wall_spline SET level=?2, wtype=?3, axis=?4, base_offset=?5, height=?6, offset0=?7, tol=?8 WHERE id=?1",
                params![id, s.level, s.wall_type, axis_json(&s.axis), s.base_offset, s.height, s.offset0, s.tol],
            )?;
            let inv = inv.then(write_pieces(db, id, s, &ps, &old, None)?);
            Ok(((), id, inv))
        })
    }

    /// Turns a chain of straight walls standing for a curve into one spline wall on `s`: the
    /// rows are reused as pieces in order (the surplus deleted), their openings move by the
    /// length parameter of the curve, and components hosted by a deleted row move to the
    /// nearest piece. `walls` run along the axis. One undo step; returns the spline wall id.
    pub fn spline_from_walls(&mut self, walls: &[i64], s: &SplineWallSpec) -> Result<i64, Error> {
        let ax = check(&self.db, s)?;
        let ps = fit(&ax, s.tol)?;
        let mut chain = 0.0;
        for &w in walls {
            refuse_piece(&self.db, w)?;
            let (ax_, ay, bx, by, bulge): (f64, f64, f64, f64, f64) = self
                .db
                .query_row("SELECT ax, ay, bx, by, bulge FROM wall WHERE id=?1", [w], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .optional()?
                .ok_or_else(|| Error::Invalid(format!("no such wall {w}")))?;
            chain += ddc_geom::arc::axis_length(v2(ax_, ay), v2(bx, by), bulge);
        }
        if walls.is_empty() || !(chain > 0.0) {
            return Err(Error::Invalid("spline wall: no walls to follow".into()));
        }
        let scale = ax.length() / chain;
        self.op("wall_spline_add", |db| {
            db.execute(
                "INSERT INTO wall_spline(level, wtype, axis, base_offset, height, offset0, tol) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![s.level, s.wall_type, axis_json(&s.axis), s.base_offset, s.height, s.offset0, s.tol],
            )?;
            let id = db.last_insert_rowid();
            let mut inv = Inv::default();
            if walls.len() > ps.len() {
                let kept = &walls[..ps.len()];
                for &w in &walls[ps.len()..] {
                    inv = inv.restore(db, "comp", "host_wall", w)?;
                    let hosted: Vec<(i64, f64, f64)> = db
                        .prepare("SELECT id, x, y FROM comp WHERE host_wall=?1")?
                        .query_map([w], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                        .collect::<Result<_, _>>()?;
                    for (c, x, y) in hosted {
                        let host = ps
                            .iter()
                            .zip(kept)
                            .map(|(p, &k)| (ddc_geom::wall_spline::piece_dist(v2(x, y), *p), k))
                            .min_by(|a, b| a.0.total_cmp(&b.0))
                            .map_or(kept[0], |h| h.1);
                        db.execute("UPDATE comp SET host_wall=?1 WHERE id=?2", params![host, c])?;
                    }
                }
            }
            let inv = inv.then(write_pieces(db, id, s, &ps, walls, Some(scale))?);
            Ok((id, id, inv.delete("wall_spline", "id", id)))
        })
    }

    /// Deletes the spline wall, its pieces and their openings. One undo step.
    pub fn delete_spline_wall(&mut self, id: i64) -> Result<(), Error> {
        self.op("wall_spline_delete", |db| {
            read(db, id)?;
            let old = piece_ids(db, id)?;
            let mut inv = Inv::default().restore(db, "wall_spline", "id", id)?;
            for &w in &old {
                inv = crate::walls::drop_wall_row(db, inv, w)?;
            }
            if !old.is_empty() {
                inv = crate::roof_edge::drop_hosts(db, inv, "wall", &old)?;
            }
            db.execute("DELETE FROM wall_spline WHERE id=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    pub fn spline_wall(&self, id: i64) -> Result<SplineWallSpec, Error> {
        read(&self.db, id)
    }

    /// Spline walls of a level (all levels with None).
    pub fn spline_walls(&self, level: Option<i64>) -> Result<Vec<(i64, SplineWallSpec)>, Error> {
        if !crate::mesh_edit::has_table(&self.db, "wall_spline")? {
            return Ok(vec![]);
        }
        let ids: Vec<i64> = self
            .db
            .prepare("SELECT id FROM wall_spline WHERE ?1 IS NULL OR level=?1 ORDER BY id")?
            .query_map([level], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        ids.into_iter().map(|id| Ok((id, read(&self.db, id)?))).collect()
    }

    /// Wall rows of the arc chain, in order along the axis.
    pub fn spline_wall_pieces(&self, id: i64) -> Result<Vec<i64>, Error> {
        piece_ids(&self.db, id)
    }

    /// The spline wall a wall row is a piece of.
    pub fn spline_of_wall(&self, wall: i64) -> Result<Option<i64>, Error> {
        if !crate::mesh_edit::has_table(&self.db, "wall_spline_piece")? {
            return Ok(None);
        }
        Ok(self
            .db
            .query_row("SELECT spline FROM wall_spline_piece WHERE wall=?1", [wall], |r| r.get(0))
            .optional()?)
    }

    /// Exact layer quantities: the spline's layer area (Gauss) in place of the chain's, the
    /// chain's join and opening effects kept (area and volume differences carried over).
    pub fn spline_wall_quantities(&self, id: i64) -> Result<Vec<SplineLayerQty>, Error> {
        let s = read(&self.db, id)?;
        let ax = SplineAxis::new(s.axis.clone()).map_err(spline_err)?;
        let ls = layers(&self.db, s.wall_type)?;
        let fs = faces(&self.db, &s)?;
        let ps: Vec<ArcPiece> = piece_ids(&self.db, id)?
            .into_iter()
            .map(|w| {
                self.db.query_row("SELECT ax, ay, bx, by, bulge FROM wall WHERE id=?1", [w], |r| {
                    Ok((v2(r.get(0)?, r.get(1)?), v2(r.get(2)?, r.get(3)?), r.get(4)?))
                })
            })
            .collect::<Result<_, _>>()?;
        let mut out = vec![];
        for (k, (material, _)) in ls.iter().enumerate() {
            // the chain's own layer area between the same faces (no joins)
            let (lo, hi) = (fs[k], fs[k + 1]);
            let chain: f64 = ps.iter().map(|p| chain_layer_area(p, lo, hi)).sum();
            let (mut area, mut vol) = (0.0, 0.0);
            for w in piece_ids(&self.db, id)? {
                let q: Vec<(f64, f64)> = self
                    .db
                    .prepare_cached("SELECT plan_area, volume FROM wall_qty WHERE wall=?1 AND pos=?2")?
                    .query_map(params![w, k as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                for (a, v) in q {
                    area += a;
                    vol += v;
                }
            }
            let d = ax.layer_area(lo, hi) - chain;
            out.push(SplineLayerQty { material: material.clone(), plan_area: area + d, volume: vol + d * s.height });
        }
        Ok(out)
    }
}

/// Plan area of an arc piece's layer between left offsets `lo` < `hi` (no joins): for an arc
/// of radius r and signed sweep th the face at offset o has radius r - sign(th) o.
fn chain_layer_area(&(a, b, bulge): &ArcPiece, lo: f64, hi: f64) -> f64 {
    let l = (b - a).len();
    if bulge == 0.0 {
        return l * (hi - lo);
    }
    let th = 4.0 * bulge.atan();
    let r = l / (2.0 * (th / 2.0).sin().abs());
    let sg = th.signum();
    0.5 * th.abs() * ((r - sg * lo).powi(2) - (r - sg * hi).powi(2)).abs()
}

#[cfg(test)]
#[path = "wall_spline_tests.rs"]
mod tests;
