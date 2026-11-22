// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Footings (schema 26): pad footings under columns (rectangular/round, stepped),
//! strip footings under walls (along the wall axis, arcs; L/T/X joints as for walls via
//! `walls::join_tolerant` within groups of equal bottom height and depth that are connected by
//! bounding boxes; strip overlaps are subtracted and raise a warning), and foundation slabs with
//! a thickened edge. Quantities live in `struct_qty`.

use std::collections::{BTreeMap, HashMap};

use ddc_geom::arc::{arc_walls, axis_length, ArcAxis, BulgeRing, CHORD_SAG};
use ddc_geom::footing::{prism, Isolated, Raft, Step};
use ddc_geom::layered::{Layer, Strip};
use ddc_geom::member::Path;
use ddc_geom::section::{Prism, Zfn};
use ddc_geom::{poly, v2, v3, Tol, V2, V3};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::struct_ext::Part;
use crate::structure::Built;
use crate::{Error, Model};

/// Pad footing.
#[derive(Clone, Debug, PartialEq)]
pub struct FootingSpec {
    pub level: i64,
    /// Host column: center below its base offset by `at`, top = column base.
    pub column: Option<i64>,
    /// Without a host: plan center; with a host: offset from the column axis.
    pub at: (f64, f64),
    /// Rotation (with a host: added to the column rotation), rad.
    pub rotation: f64,
    pub round: bool,
    /// Steps bottom to top (the base is first); for a circle `w` is the diameter.
    pub steps: Vec<Step>,
    /// Without a host (or the column is deleted): top = level elevation + top_offset.
    pub top_offset: f64,
    pub material: String,
}

/// Strip footing.
#[derive(Clone, Debug, PartialEq)]
pub struct StripFootingSpec {
    pub level: i64,
    /// Host wall: wall axis and base; the footing's own axis is a fallback if the wall is gone.
    pub wall: Option<i64>,
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub bulge: f64,
    pub width: f64,
    pub depth: f64,
    /// Footing axis shift to the left (from the wall mid-thickness or from its own axis).
    pub ecc: f64,
    /// Top: from the host wall base; without a host, from the level.
    pub top_offset: f64,
    pub material: String,
}

/// Axis of a strip footing in plan: an arc piece (bulge 0 is a line), or the left offset of
/// the axis of spline wall `spline` when the host wall is one of its pieces.
#[derive(Clone, Debug, PartialEq)]
pub enum StripAxis {
    Arc { a: (f64, f64), b: (f64, f64), bulge: f64 },
    Spline { spline: i64, offset: f64 },
}

/// Foundation slab with a thickened edge (`edge_depth` = 0 means no thickening).
#[derive(Clone, Debug, PartialEq)]
pub struct RaftSpec {
    pub level: i64,
    pub ring: Vec<(f64, f64)>,
    pub thickness: f64,
    pub edge_width: f64,
    pub edge_depth: f64,
    /// Top = level elevation + top_offset.
    pub top_offset: f64,
    pub material: String,
}

pub const CONCRETE: &str = "Concrete";

/// Offset (ex, ey) in axes rotated by `a`.
fn turn(v: (f64, f64), a: f64) -> V2 {
    let (c, s) = (a.cos(), a.sin());
    v2(v.0 * c - v.1 * s, v.0 * s + v.1 * c)
}

/// Axis a->b (an arc via bulge) shifted left by `o`: the arc stays concentric.
pub(crate) fn shift_left(a: V2, b: V2, bulge: f64, o: f64) -> (V2, V2) {
    if o == 0.0 {
        return (a, b);
    }
    let left = |t: V2| v2(-t.y, t.x) * o;
    match ArcAxis::new(a, b, bulge, Tol::default()) {
        Ok(ax) => (
            a + left(ax.tangent_at(0.0)),
            b + left(ax.tangent_at(ax.length())),
        ),
        Err(_) => {
            let d = b - a;
            let l = d.len();
            if l < 1e-12 {
                return (a, b);
            }
            let t = d * (1.0 / l);
            (a + left(t), b + left(t))
        }
    }
}

fn differ(a: f64, b: f64) -> bool {
    (a - b).abs() > 1e-12
}

pub(crate) fn flat_prism(rings: Vec<Vec<V2>>, z0: f64, z1: f64) -> Prism {
    Prism {
        rings,
        bottom: Zfn::flat(z0),
        top: Zfn::flat(z1),
        voids: vec![],
    }
}

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    let d = b - a;
    let l2 = d.dot(d);
    let t = if l2 < 1e-24 {
        0.0
    } else {
        ((p - a).dot(d) / l2).clamp(0.0, 1.0)
    };
    (a + d * t - p).len()
}

fn on_ring(p: V2, r: &[V2]) -> bool {
    (0..r.len()).any(|k| seg_dist(p, r[k], r[(k + 1) % r.len()]) < 1e-6)
}

/// Strip edges from plan pieces (rings, joint inserts) without seams between pieces: an edge
/// whose midpoint lies on the boundary of another piece is interior.
fn outline_lines(pieces: &[Vec<V2>], z0: f64, z1: f64) -> Vec<(V3, V3)> {
    let other = |i: usize, p: V2| {
        pieces
            .iter()
            .enumerate()
            .any(|(j, r)| j != i && on_ring(p, r))
    };
    let mut out = vec![];
    for (i, r) in pieces.iter().enumerate() {
        let n = r.len();
        for k in 0..n {
            let (o, p, q) = (r[(k + n - 1) % n], r[k], r[(k + 1) % n]);
            if (q - p).len() < 1e-9 || other(i, (p + q) * 0.5) {
                continue;
            }
            for z in [z0, z1] {
                out.push((v3(p.x, p.y, z), v3(q.x, q.y, z)));
            }
            // vertical edge at a corner (not on a smooth arc)
            let (d0, d1) = (p - o, q - p);
            let sin = d0.cross(d1).abs() / (d0.len() * d1.len()).max(1e-300);
            if sin > 0.26 && !other(i, p) {
                out.push((v3(p.x, p.y, z0), v3(p.x, p.y, z1)));
            }
        }
    }
    out
}

fn invalid<T>(m: String) -> Result<T, Error> {
    Err(Error::Invalid(m))
}

fn level_ok(db: &Connection, level: i64) -> Result<(), Error> {
    let n: i64 = db.query_row("SELECT count(*) FROM level WHERE id=?1", [level], |r| {
        r.get(0)
    })?;
    if n == 0 {
        return invalid(format!("no such level {level}"));
    }
    Ok(())
}

pub fn steps_text(s: &[Step]) -> String {
    s.iter()
        .map(|s| format!("{} {} {}", s.w, s.l, s.h))
        .collect::<Vec<_>>()
        .join(";")
}

pub fn parse_steps(s: &str) -> Option<Vec<Step>> {
    s.split(';')
        .map(|q| {
            let v: Vec<f64> = q
                .split(' ')
                .map(|x| x.parse().ok())
                .collect::<Option<_>>()?;
            match v.as_slice() {
                &[w, l, h] => Some(Step { w, l, h }),
                _ => None,
            }
        })
        .collect()
}

fn check_footing(db: &Connection, s: &FootingSpec) -> Result<(), Error> {
    level_ok(db, s.level)?;
    if let Some(c) = s.column {
        let n: i64 = db.query_row("SELECT count(*) FROM col WHERE id=?1", [c], |r| r.get(0))?;
        if n == 0 {
            return invalid(format!("footing: no such column {c}"));
        }
    }
    let probe = Isolated {
        round: s.round,
        steps: s.steps.clone(),
        center: v2(s.at.0, s.at.1),
        rotation: s.rotation,
        top: s.top_offset,
    };
    probe.check().map_err(Error::Invalid)
}

fn check_strip(db: &Connection, s: &StripFootingSpec) -> Result<(), Error> {
    level_ok(db, s.level)?;
    let v = [
        s.a.0,
        s.a.1,
        s.b.0,
        s.b.1,
        s.bulge,
        s.width,
        s.depth,
        s.ecc,
        s.top_offset,
    ];
    if !v.iter().all(|x| x.is_finite()) || s.width <= 0.0 || s.depth <= 0.0 {
        return invalid("strip footing: width and depth must be positive".into());
    }
    crate::walls::check_len("strip footing", s.a, s.b)?;
    if s.bulge != 0.0 {
        crate::walls::check_bulge(s.a, s.b, s.bulge)?;
    }
    if let Some(w) = s.wall {
        let n: i64 = db.query_row("SELECT count(*) FROM wall WHERE id=?1", [w], |r| r.get(0))?;
        if n == 0 {
            return invalid(format!("strip footing: no such wall {w}"));
        }
    }
    Ok(())
}

fn raft_geom(s: &RaftSpec, top: f64) -> Raft {
    Raft {
        ring: s.ring.iter().map(|p| v2(p.0, p.1)).collect(),
        top,
        thickness: s.thickness,
        edge_width: s.edge_width,
        edge_depth: s.edge_depth,
    }
}

fn check_raft(db: &Connection, s: &RaftSpec) -> Result<(), Error> {
    level_ok(db, s.level)?;
    raft_geom(s, s.top_offset).check().map_err(Error::Invalid)
}

fn insert_footing(db: &Connection, s: &FootingSpec, id: Option<i64>) -> Result<i64, Error> {
    // with a host, (x, y, frot) is a fallback position; regeneration keeps it in sync with the column
    let (x, y, frot) = match s.column {
        Some(c) => {
            let p: (f64, f64, f64) =
                db.query_row("SELECT x, y, rotation FROM col WHERE id=?1", [c], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
            let o = turn(s.at, p.2);
            (p.0 + o.x, p.1 + o.y, p.2 + s.rotation)
        }
        None => (s.at.0, s.at.1, s.rotation),
    };
    let (ex, ey) = if s.column.is_some() { s.at } else { (0.0, 0.0) };
    db.execute(
        "INSERT OR REPLACE INTO iso_footing(id,level,col,x,y,ex,ey,rotation,round,steps,top_offset,\
         material,frot,ftop) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?11)",
        params![
            id,
            s.level,
            s.column,
            x,
            y,
            ex,
            ey,
            s.rotation,
            s.round as i64,
            steps_text(&s.steps),
            s.top_offset,
            crate::material_look::assign("footing", &s.material, ""),
            frot
        ],
    )?;
    Ok(db.last_insert_rowid())
}

fn insert_strip(db: &Connection, s: &StripFootingSpec, id: Option<i64>) -> Result<i64, Error> {
    db.execute(
        "INSERT OR REPLACE INTO strip_footing(id,level,wall,ax,ay,bx,by,bulge,width,depth,ecc,\
         top_offset,material,ftop) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?12)",
        params![
            id,
            s.level,
            s.wall,
            s.a.0,
            s.a.1,
            s.b.0,
            s.b.1,
            s.bulge,
            s.width,
            s.depth,
            s.ecc,
            s.top_offset,
            crate::material_look::assign("footing", &s.material, "")
        ],
    )?;
    Ok(db.last_insert_rowid())
}

fn insert_raft(db: &Connection, s: &RaftSpec, id: Option<i64>) -> Result<i64, Error> {
    db.execute(
        "INSERT OR REPLACE INTO raft(id,level,thickness,edge_width,edge_depth,top_offset,material) \
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            id,
            s.level,
            s.thickness,
            s.edge_width,
            s.edge_depth,
            s.top_offset,
            crate::material_look::assign("raft", &s.material, "")
        ],
    )?;
    let rid = db.last_insert_rowid();
    db.execute("DELETE FROM raft_vertex WHERE raft=?1", [rid])?;
    for (i, p) in s.ring.iter().enumerate() {
        db.execute(
            "INSERT INTO raft_vertex(raft, idx, x, y) VALUES (?1,?2,?3,?4)",
            params![rid, i as i64, p.0, p.1],
        )?;
    }
    Ok(rid)
}

fn footing_row(r: &rusqlite::Row) -> rusqlite::Result<(i64, FootingSpec)> {
    let col: Option<i64> = r.get(2)?;
    let xy: (f64, f64) = (r.get(3)?, r.get(4)?);
    let e: (f64, f64) = (r.get(5)?, r.get(6)?);
    let steps: String = r.get(9)?;
    Ok((
        r.get(0)?,
        FootingSpec {
            level: r.get(1)?,
            column: col,
            at: if col.is_some() { e } else { xy },
            rotation: r.get(7)?,
            round: r.get::<_, i64>(8)? != 0,
            steps: parse_steps(&steps).unwrap_or_default(),
            top_offset: r.get(10)?,
            material: r.get(11)?,
        },
    ))
}

const FOOTING_SEL: &str =
    "SELECT id,level,col,x,y,ex,ey,rotation,round,steps,top_offset,material FROM iso_footing";
const STRIP_SEL: &str =
    "SELECT id,level,wall,ax,ay,bx,by,bulge,width,depth,ecc,top_offset,material FROM strip_footing";

fn strip_row(r: &rusqlite::Row) -> rusqlite::Result<(i64, StripFootingSpec)> {
    Ok((
        r.get(0)?,
        StripFootingSpec {
            level: r.get(1)?,
            wall: r.get(2)?,
            a: (r.get(3)?, r.get(4)?),
            b: (r.get(5)?, r.get(6)?),
            bulge: r.get(7)?,
            width: r.get(8)?,
            depth: r.get(9)?,
            ecc: r.get(10)?,
            top_offset: r.get(11)?,
            material: r.get(12)?,
        },
    ))
}

fn raft_spec(db: &Connection, id: i64) -> Result<RaftSpec, Error> {
    let mut s = db
        .query_row(
            "SELECT level,thickness,edge_width,edge_depth,top_offset,material FROM raft WHERE id=?1",
            [id],
            |r| {
                Ok(RaftSpec {
                    level: r.get(0)?,
                    ring: vec![],
                    thickness: r.get(1)?,
                    edge_width: r.get(2)?,
                    edge_depth: r.get(3)?,
                    top_offset: r.get(4)?,
                    material: r.get(5)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no such raft {id}")))?;
    let mut st = db.prepare("SELECT x, y FROM raft_vertex WHERE raft=?1 ORDER BY idx")?;
    s.ring = st
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(s)
}

/// Wall for a strip: level, axis, bulge, base offset from the level, mid-thickness offset.
struct WallAxis {
    level: i64,
    a: V2,
    b: V2,
    bulge: f64,
    base_offset: f64,
    center: f64,
}

fn wall_axes(db: &Connection) -> Result<HashMap<i64, WallAxis>, Error> {
    let mut st = db.prepare(
        "SELECT w.id, w.level, w.ax, w.ay, w.bx, w.by, w.bulge, w.base_offset, w.offset0, \
         (SELECT total(l.thickness) FROM wall_type_layer l WHERE l.wtype = w.wtype) FROM wall w",
    )?;
    let rows = st.query_map([], |r| {
        let total: f64 = r.get(9)?;
        let o0: Option<f64> = r.get(8)?;
        Ok((
            r.get::<_, i64>(0)?,
            WallAxis {
                level: r.get(1)?,
                a: v2(r.get(2)?, r.get(3)?),
                b: v2(r.get(4)?, r.get(5)?),
                bulge: r.get(6)?,
                base_offset: r.get(7)?,
                center: o0.unwrap_or(-total / 2.0) + total / 2.0,
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn part(kind: &'static str, id: i64, level: i64, material: &str) -> Part {
    Part::new(kind, id, level, kind, material)
}

fn col_bases(members: &[Built]) -> HashMap<i64, (V3, f64)> {
    members
        .iter()
        .filter(|b| b.kind == "column")
        .map(|b| {
            let a = match b.m.path {
                Path::Line { a, .. } | Path::Arc { a, .. } => a,
            };
            (b.id, (a, b.m.rotation))
        })
        .collect()
}

/// Fallback geometry of hosted footings = follow the host now; if the host is deleted, the
/// footing is detached and stays in place (column and wall ids are reused).
/// Returns the inverse commands (row snapshots before the edit).
pub(crate) fn sync_hosts(db: &Connection, members: &[Built]) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    let hosted: i64 = db.query_row(
        "SELECT (SELECT count(*) FROM iso_footing WHERE col IS NOT NULL) + \
         (SELECT count(*) FROM strip_footing WHERE wall IS NOT NULL)",
        [],
        |r| r.get(0),
    )?;
    if hosted == 0 {
        return Ok(inv);
    }
    let lv: HashMap<i64, f64> = crate::walls::levels(db)?.into_iter().collect();
    let elev = |l: i64| lv.get(&l).copied().unwrap_or(0.0);
    let cols = col_bases(members);
    type IsoRow = (i64, i64, i64, f64, f64, f64, f64, f64, f64, f64);
    let rows: Vec<IsoRow> = {
        let mut st = db.prepare(
            "SELECT id, level, col, x, y, ex, ey, rotation, frot, ftop FROM iso_footing \
             WHERE col IS NOT NULL ORDER BY id",
        )?;
        let r = st.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        })?;
        r.collect::<Result<_, _>>()?
    };
    for (id, level, col, x, y, ex, ey, rotation, frot, top) in rows {
        match cols.get(&col) {
            Some((base, rot)) => {
                let c = v2(base.x, base.y) + turn((ex, ey), *rot);
                let (fr, ft) = (rot + rotation, base.z - elev(level));
                if differ(x, c.x) || differ(y, c.y) || differ(frot, fr) || differ(top, ft) {
                    inv = inv.restore(db, "iso_footing", "id", id)?;
                    db.execute(
                        "UPDATE iso_footing SET x=?2, y=?3, frot=?4, ftop=?5 WHERE id=?1",
                        params![id, c.x, c.y, fr, ft],
                    )?;
                }
            }
            None => {
                inv = inv.restore(db, "iso_footing", "id", id)?;
                db.execute(
                    "UPDATE iso_footing SET col=NULL, ex=0, ey=0, rotation=frot, top_offset=ftop \
                     WHERE id=?1",
                    [id],
                )?;
            }
        }
    }
    let walls = wall_axes(db)?;
    type StripRow = (i64, i64, i64, [f64; 5], f64, f64);
    let rows: Vec<StripRow> = {
        let mut st = db.prepare(
            "SELECT id, level, wall, ax, ay, bx, by, bulge, ftop, top_offset FROM strip_footing \
             WHERE wall IS NOT NULL ORDER BY id",
        )?;
        let r = st.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                [r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?],
                r.get(8)?,
                r.get(9)?,
            ))
        })?;
        r.collect::<Result<_, _>>()?
    };
    for (id, level, wall, g, ftop, top) in rows {
        match walls.get(&wall) {
            Some(w) => {
                let (fa, fb) = shift_left(w.a, w.b, w.bulge, w.center);
                let ft = elev(w.level) + w.base_offset + top - elev(level);
                let now = [fa.x, fa.y, fb.x, fb.y, w.bulge];
                if g.iter().zip(&now).any(|(p, q)| differ(*p, *q)) || differ(ftop, ft) {
                    inv = inv.restore(db, "strip_footing", "id", id)?;
                    db.execute(
                        "UPDATE strip_footing SET ax=?2, ay=?3, bx=?4, by=?5, bulge=?6, ftop=?7 \
                         WHERE id=?1",
                        params![id, now[0], now[1], now[2], now[3], now[4], ft],
                    )?;
                }
            }
            None => {
                inv = inv.restore(db, "strip_footing", "id", id)?;
                db.execute(
                    "UPDATE strip_footing SET wall=NULL, top_offset=ftop WHERE id=?1",
                    [id],
                )?;
            }
        }
    }
    Ok(inv)
}

/// Strip footings of a spline wall when its arc chain is rewritten (`old` pieces, then `new`):
/// a reused wall row keeps its footing ([`sync_hosts`] moves it), a new piece gets a copy of the
/// footing of the last footed old piece, a dropped piece takes its footing along. Returns the
/// inverse; runs inside the reshaping op, so it is the same undo step.
pub(crate) fn follow_pieces(db: &Connection, old: &[i64], new: &[i64]) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    if !crate::mesh_edit::has_table(db, "strip_footing")? {
        return Ok(inv);
    }
    let mut tpl = None;
    for &w in old {
        let f: Option<i64> = db.query_row(
            "SELECT min(id) FROM strip_footing WHERE wall=?1",
            [w],
            |r| r.get(0),
        )?;
        tpl = f.or(tpl);
    }
    let Some(tpl) = tpl else {
        return Ok(inv);
    };
    for &w in new.iter().skip(old.len()) {
        db.execute(
            "INSERT INTO strip_footing(level,wall,ax,ay,bx,by,bulge,width,depth,ecc,top_offset,\
             material,ftop) SELECT level,?2,ax,ay,bx,by,bulge,width,depth,ecc,top_offset,material,\
             ftop FROM strip_footing WHERE id=?1",
            params![tpl, w],
        )?;
        inv = inv.delete("strip_footing", "id", db.last_insert_rowid());
    }
    for &w in old.iter().skip(new.len()) {
        inv = inv.restore(db, "strip_footing", "wall", w)?;
        db.execute("DELETE FROM strip_footing WHERE wall=?1", [w])?;
    }
    Ok(inv)
}

fn ring_area(r: &BulgeRing) -> f64 {
    r.area().abs()
}

/// Footings: pad, strip (joints per level), slabs.
/// Part length: pad = height, strip = axis length, slab = perimeter.
pub(crate) fn build(db: &Connection, members: &[Built]) -> Result<Vec<Part>, Error> {
    let lv: HashMap<i64, f64> = crate::walls::levels(db)?.into_iter().collect();
    let cols = col_bases(members);
    let mut out = vec![];
    let mut st = db.prepare(&format!("{FOOTING_SEL} ORDER BY id"))?;
    let rows: Vec<(i64, FootingSpec, (f64, f64))> = st
        .query_map([], |r| {
            let (id, s) = footing_row(r)?;
            Ok((id, s, (r.get(3)?, r.get(4)?)))
        })?
        .collect::<Result<_, _>>()?;
    for (id, s, xy) in rows {
        let elev = lv.get(&s.level).copied().unwrap_or(0.0);
        let host = s.column.and_then(|c| cols.get(&c));
        // offset `at` is in column axes: the eccentricity rotates together with the column
        let (center, rotation, top) = match host {
            Some((base, rot)) => (
                v2(base.x, base.y) + turn(s.at, *rot),
                rot + s.rotation,
                base.z,
            ),
            None => (v2(xy.0, xy.1), s.rotation, elev + s.top_offset),
        };
        let f = Isolated {
            round: s.round,
            steps: s.steps.clone(),
            center,
            rotation,
            top,
        };
        if f.check().is_err() {
            continue;
        }
        let q = f.quantities();
        let mut p = part("footing", id, s.level, &s.material);
        p.meshes = f.meshes();
        p.length = q.depth;
        p.volume = q.volume;
        p.area = q.base_area;
        let s0 = f.steps[0];
        p.size = [s0.w, if f.round { s0.w } else { s0.l }, q.depth];
        p.plan = vec![f.outline(0)];
        let mut z = f.bottom();
        for k in 0..f.steps.len() {
            let h = f.steps[k].h;
            p.prisms.push(flat_prism(vec![f.outline(k)], z, z + h));
            z += h;
        }
        out.push(p);
    }
    out.extend(strips(db, &lv)?);
    let ids: Vec<(i64, i64)> = {
        let mut st = db.prepare("SELECT id, level FROM raft ORDER BY id")?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        r.collect::<Result<_, _>>()?
    };
    for (id, level) in ids {
        let s = raft_spec(db, id)?;
        let g = raft_geom(&s, lv.get(&level).copied().unwrap_or(0.0) + s.top_offset);
        if g.check().is_err() {
            continue;
        }
        let q = g.quantities();
        let mut p = part("raft", id, level, &s.material);
        p.meshes = g.meshes();
        p.length = poly::perimeter(&g.ring);
        p.volume = q.volume;
        p.area = q.base_area;
        let mut outer = g.ring.clone();
        if poly::area(&outer) < 0.0 {
            outer.reverse();
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for v in &outer {
            (x0, y0, x1, y1) = (x0.min(v.x), y0.min(v.y), x1.max(v.x), y1.max(v.y));
        }
        let rib = if g.thickened() { g.edge_depth } else { 0.0 };
        p.size = [x1 - x0, y1 - y0, g.thickness + rib];
        p.plan = vec![outer.clone()];
        let under = g.top - g.thickness;
        p.prisms.push(flat_prism(vec![outer.clone()], under, g.top));
        if rib > 0.0 {
            if let Some(mut inner) = g.inner() {
                if poly::area(&inner) > 0.0 {
                    inner.reverse();
                }
                p.prisms
                    .push(flat_prism(vec![outer, inner], under - rib, under));
            }
        }
        out.push(p);
    }
    Ok(out)
}

/// Contour bounding box (x0, y0, x1, y1).
fn bbox(rs: &[Vec<V2>]) -> [f64; 4] {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in rs.iter().flatten() {
        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
    }
    b
}

fn meet(a: [f64; 4], b: [f64; 4], pad: f64) -> bool {
    a[0] <= b[2] + pad && b[0] <= a[2] + pad && a[1] <= b[3] + pad && b[1] <= a[3] + pad
}

/// Index subsets connected by bounding boxes (with margin `pad`).
fn components(boxes: &[[f64; 4]], pad: f64) -> Vec<Vec<usize>> {
    let n = boxes.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| boxes[a][0].total_cmp(&boxes[b][0]));
    for (k, &i) in order.iter().enumerate() {
        for &j in &order[k + 1..] {
            if boxes[j][0] > boxes[i][2] + pad {
                break;
            }
            if meet(boxes[i], boxes[j], pad) {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                parent[ri] = rj;
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out: Vec<Vec<usize>> = groups.into_values().collect();
    out.sort_by_key(|g| g[0]);
    out
}

/// Strips of a level are joined in plan like walls (a single layer of strip width), within groups
/// of equal bottom height and depth (otherwise a miter at different elevations), connected by
/// bounding boxes (joining cost scales with group size). Overlap of strips (no joint, different
/// groups) is subtracted from the later one by id and raises a warning.
fn strips(db: &Connection, lv: &HashMap<i64, f64>) -> Result<Vec<Part>, Error> {
    let mut st = db.prepare(&format!("{STRIP_SEL} ORDER BY id"))?;
    let rows: Vec<(i64, StripFootingSpec)> =
        st.query_map([], strip_row)?.collect::<Result<_, _>>()?;
    if rows.is_empty() {
        return Ok(vec![]);
    }
    let walls = wall_axes(db)?;
    let tol = Tol::default();
    // geometry of each strip: band, bulge, strip mid-axis
    let mut strips: Vec<Strip> = vec![];
    let mut bulges: Vec<f64> = vec![];
    let mut mids: Vec<(V2, V2)> = vec![];
    for (_, s) in &rows {
        let elev = lv.get(&s.level).copied().unwrap_or(0.0);
        let host = s.wall.and_then(|w| walls.get(&w));
        let (a, b, bulge, center, top) = match host {
            Some(w) => (
                w.a,
                w.b,
                w.bulge,
                w.center,
                lv.get(&w.level).copied().unwrap_or(0.0) + w.base_offset + s.top_offset,
            ),
            None => (
                v2(s.a.0, s.a.1),
                v2(s.b.0, s.b.1),
                s.bulge,
                0.0,
                elev + s.top_offset,
            ),
        };
        strips.push(Strip {
            a,
            b,
            layers: vec![Layer::new(s.width)],
            offset0: center + s.ecc - s.width / 2.0,
            base: top - s.depth,
            height: s.depth,
        });
        bulges.push(bulge);
        mids.push(shift_left(a, b, bulge, center + s.ecc));
    }
    let q = |x: f64| (x * 1e6).round() as i64;
    let mut groups: BTreeMap<(i64, i64, i64), Vec<usize>> = BTreeMap::new();
    for (i, (_, s)) in rows.iter().enumerate() {
        let k = (s.level, q(strips[i].base), q(strips[i].height));
        groups.entry(k).or_default().push(i);
    }
    let n = rows.len();
    let mut pieces: Vec<Vec<Vec<V2>>> = vec![vec![]; n];
    // exact area (arcs by circle segments, not by the polyline)
    let mut exact = vec![0.0; n];
    let mut warns: Vec<Vec<String>> = vec![vec![]; n];
    for idx in groups.values() {
        let boxes: Vec<[f64; 4]> = idx
            .iter()
            .map(|&i| {
                let s = &strips[i];
                let reach = s
                    .offset0
                    .abs()
                    .max((s.offset0 + s.layers[0].thickness).abs());
                let b: [f64; 4] = match ArcAxis::new(s.a, s.b, bulges[i], tol) {
                    Ok(ax) => ax.bbox(0.0),
                    Err(_) => bbox(&[vec![s.a, s.b]]),
                };
                [b[0] - reach, b[1] - reach, b[2] + reach, b[3] + reach]
            })
            .collect();
        for comp in components(&boxes, 1e-6) {
            let ks: Vec<usize> = comp.iter().map(|&c| idx[c]).collect();
            let ss: Vec<Strip> = ks.iter().map(|&i| strips[i].clone()).collect();
            let bs: Vec<f64> = ks.iter().map(|&i| bulges[i]).collect();
            let mut rings: Vec<Option<Vec<BulgeRing>>> = vec![None; ks.len()];
            let mut fillers: Vec<(Vec<V2>, usize)> = vec![];
            match crate::walls::join_tolerant(&ss, &bs, &[], tol) {
                Ok((j, pairs)) => {
                    for (k, r) in j.rings.into_iter().enumerate() {
                        rings[k] = r.into_iter().next();
                    }
                    for (f, (w, _)) in j.fillers.into_iter().zip(j.filler_owners) {
                        fillers.push((f, w));
                    }
                    for (a, _) in pairs.into_iter().filter(|p| p.0 == p.1) {
                        warns[ks[a]].push("strip footing: degenerate junction".into());
                    }
                }
                Err(_) => {
                    // the network does not join: each strip stands alone with square ends
                    for k in 0..ks.len() {
                        if let Ok(mut j) = arc_walls(&ss[k..=k], &bs[k..=k], tol) {
                            rings[k] = j.rings.remove(0).into_iter().next();
                        }
                    }
                }
            }
            for (k, &i) in ks.iter().enumerate() {
                let Some(rs) = &rings[k] else { continue };
                exact[i] += rs.iter().map(ring_area).sum::<f64>();
                pieces[i].extend(rs.iter().map(|r| r.tessellate(CHORD_SAG)));
                for (f, _) in fillers.iter().filter(|f| f.1 == k) {
                    exact[i] += poly::area(f).abs();
                    pieces[i].push(f.clone());
                }
            }
        }
    }
    // overlaps: plan x height; subtracted from the later strip
    let boxes: Vec<[f64; 4]> = pieces.iter().map(|p| bbox(p)).collect();
    let convex: Vec<Vec<Vec<V2>>> = pieces
        .iter()
        .map(|p| p.iter().flat_map(|r| poly::convex_pieces(r)).collect())
        .collect();
    let mut cut = vec![(0.0, 0.0); n];
    let mut order: Vec<usize> = (0..n).filter(|&i| !pieces[i].is_empty()).collect();
    order.sort_by(|&a, &b| boxes[a][0].total_cmp(&boxes[b][0]));
    for (k, &i) in order.iter().enumerate() {
        for &j in &order[k + 1..] {
            if boxes[j][0] > boxes[i][2] {
                break;
            }
            if rows[i].1.level != rows[j].1.level || !meet(boxes[i], boxes[j], 0.0) {
                continue;
            }
            let (si, sj) = (&strips[i], &strips[j]);
            let dz = (si.base + si.height).min(sj.base + sj.height) - si.base.max(sj.base);
            if dz <= 1e-9 {
                continue;
            }
            let (late, early) = if i > j { (i, j) } else { (j, i) };
            let ov: f64 = pieces[late]
                .iter()
                .map(|r| poly::overlap_area(r, &convex[early]).abs())
                .sum();
            if ov > 1e-6 {
                cut[late].0 += ov;
                cut[late].1 += ov * dz;
                let other = rows[early].0;
                warns[late].push(format!("strip footing overlaps strip footing {other}"));
            }
        }
    }
    let mut out = vec![];
    for i in 0..n {
        if pieces[i].is_empty() {
            continue;
        }
        let (id, s) = (rows[i].0, &rows[i].1);
        let st = &strips[i];
        let (z0, z1) = (st.base, st.base + st.height);
        let mut p = part("strip_footing", id, s.level, &s.material);
        let area = exact[i] - cut[i].0;
        p.meshes = pieces[i].iter().map(|r| prism(r, z0, z1)).collect();
        p.prisms = pieces[i]
            .iter()
            .map(|r| flat_prism(vec![r.clone()], z0, z1))
            .collect();
        p.lines = Some(outline_lines(&pieces[i], z0, z1));
        p.length = axis_length(mids[i].0, mids[i].1, bulges[i]);
        p.area = area.max(0.0);
        p.volume = (exact[i] * st.height - cut[i].1).max(0.0);
        p.size = [p.length, s.width, s.depth];
        p.plan = pieces[i].clone();
        p.warn = std::mem::take(&mut warns[i]);
        out.push(p);
    }
    Ok(out)
}

impl Model {
    pub fn add_footing(&mut self, s: &FootingSpec) -> Result<i64, Error> {
        Ok(self.add_footings(std::slice::from_ref(s))?[0])
    }

    /// Several pad footings in one operation (one undo).
    pub fn add_footings(&mut self, ss: &[FootingSpec]) -> Result<Vec<i64>, Error> {
        for s in ss {
            check_footing(&self.db, s)?;
        }
        self.op("footing_iso_add", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            for s in ss {
                let id = insert_footing(db, s, None)?;
                inv = inv.delete("iso_footing", "id", id);
                ids.push(id);
            }
            let first = ids.first().copied().unwrap_or(0);
            Ok((ids, first, inv))
        })
    }

    /// Footings under columns from a template (`template.at` is the offset from the column axis):
    /// level and top follow the column; columns that already have a footing are skipped. One undo.
    pub fn footings_under_columns(
        &mut self,
        columns: &[i64],
        template: &FootingSpec,
    ) -> Result<Vec<i64>, Error> {
        let mut ss = vec![];
        for &c in columns {
            let has: i64 =
                self.db
                    .query_row("SELECT count(*) FROM iso_footing WHERE col=?1", [c], |r| {
                        r.get(0)
                    })?;
            if has > 0 || ss.iter().any(|s: &FootingSpec| s.column == Some(c)) {
                continue;
            }
            let col = self.column(c)?;
            ss.push(FootingSpec {
                level: col.level,
                column: Some(c),
                top_offset: col.base_offset,
                ..template.clone()
            });
        }
        if ss.is_empty() {
            return Ok(vec![]);
        }
        self.add_footings(&ss)
    }

    pub fn update_footing(&mut self, id: i64, s: &FootingSpec) -> Result<(), Error> {
        check_footing(&self.db, s)?;
        self.op("footing_iso_edit", |db| {
            let inv = Inv::default().restore(db, "iso_footing", "id", id)?;
            let n: i64 =
                db.query_row("SELECT count(*) FROM iso_footing WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
            if n == 0 {
                return invalid(format!("no such footing {id}"));
            }
            insert_footing(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    /// Isolated footings standing under column `col`.
    pub fn footings_of_column(&self, col: i64) -> Result<Vec<i64>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id FROM iso_footing WHERE col=?1 ORDER BY id")?;
        let v = st
            .query_map([col], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }

    pub fn footing(&self, id: i64) -> Result<FootingSpec, Error> {
        Ok(self
            .db
            .query_row(&format!("{FOOTING_SEL} WHERE id=?1"), [id], footing_row)?
            .1)
    }

    pub fn add_strip_footings(&mut self, ss: &[StripFootingSpec]) -> Result<Vec<i64>, Error> {
        for s in ss {
            check_strip(&self.db, s)?;
        }
        self.op("footing_strip_add", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            for s in ss {
                let id = insert_strip(db, s, None)?;
                inv = inv.delete("strip_footing", "id", id);
                ids.push(id);
            }
            let first = ids.first().copied().unwrap_or(0);
            Ok((ids, first, inv))
        })
    }

    /// Strips under walls (along the axis, wall base = strip top); walls that already have a strip are skipped. One undo.
    pub fn strip_footings_under_walls(
        &mut self,
        walls: &[i64],
        width: f64,
        depth: f64,
        material: &str,
    ) -> Result<Vec<i64>, Error> {
        let axes = wall_axes(&self.db)?;
        // a piece of a spline wall stands for the whole curve: one footing per arc piece,
        // concentric with it, so the band follows the spline within its fit tolerance
        let mut all: Vec<i64> = vec![];
        for &w in walls {
            let group = crate::wall_spline::group_items(&self.db, "wall", w)?;
            let pieces: Vec<i64> = group.iter().filter(|g| g.0 == "wall").map(|g| g.1).collect();
            for p in if pieces.is_empty() { vec![w] } else { pieces } {
                if !all.contains(&p) {
                    all.push(p);
                }
            }
        }
        let mut ss: Vec<StripFootingSpec> = vec![];
        for &w in &all {
            let Some(ax) = axes.get(&w) else {
                return invalid(format!("no such wall {w}"));
            };
            let has: i64 = self.db.query_row(
                "SELECT count(*) FROM strip_footing WHERE wall=?1",
                [w],
                |r| r.get(0),
            )?;
            if has > 0 || ss.iter().any(|s| s.wall == Some(w)) {
                continue;
            }
            ss.push(StripFootingSpec {
                level: ax.level,
                wall: Some(w),
                // fallback axis = wall mid-thickness (as regeneration keeps it)
                a: {
                    let (fa, _) = shift_left(ax.a, ax.b, ax.bulge, ax.center);
                    (fa.x, fa.y)
                },
                b: {
                    let (_, fb) = shift_left(ax.a, ax.b, ax.bulge, ax.center);
                    (fb.x, fb.y)
                },
                bulge: ax.bulge,
                width,
                depth,
                ecc: 0.0,
                top_offset: 0.0,
                material: material.to_string(),
            });
        }
        if ss.is_empty() {
            return Ok(vec![]);
        }
        self.add_strip_footings(&ss)
    }

    pub fn update_strip_footing(&mut self, id: i64, s: &StripFootingSpec) -> Result<(), Error> {
        check_strip(&self.db, s)?;
        self.op("footing_strip_edit", |db| {
            let inv = Inv::default().restore(db, "strip_footing", "id", id)?;
            let n: i64 = db.query_row(
                "SELECT count(*) FROM strip_footing WHERE id=?1",
                [id],
                |r| r.get(0),
            )?;
            if n == 0 {
                return invalid(format!("no such strip footing {id}"));
            }
            insert_strip(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn strip_footing(&self, id: i64) -> Result<StripFootingSpec, Error> {
        Ok(self
            .db
            .query_row(&format!("{STRIP_SEL} WHERE id=?1"), [id], strip_row)?
            .1)
    }

    /// The axis the band is swept along (middle of its width), as built.
    pub fn strip_footing_axis(&self, id: i64) -> Result<StripAxis, Error> {
        self.strip_axis(id, true)
    }

    /// The arc piece of a strip footing, also when its host is a piece of a spline wall.
    pub fn strip_footing_arc(&self, id: i64) -> Result<StripAxis, Error> {
        self.strip_axis(id, false)
    }

    fn strip_axis(&self, id: i64, whole_spline: bool) -> Result<StripAxis, Error> {
        let s = self.strip_footing(id)?;
        let walls = wall_axes(&self.db)?;
        if let Some((w, ax)) = s.wall.and_then(|w| walls.get(&w).map(|ax| (w, ax))) {
            let offset = ax.center + s.ecc;
            let group = if whole_spline {
                crate::wall_spline::group_items(&self.db, "wall", w)?
            } else {
                vec![]
            };
            if let Some(spline) = group
                .into_iter()
                .find(|g| g.0 == "wall_spline")
                .map(|g| g.1)
            {
                return Ok(StripAxis::Spline { spline, offset });
            }
            let (a, b) = shift_left(ax.a, ax.b, ax.bulge, offset);
            return Ok(StripAxis::Arc { a: (a.x, a.y), b: (b.x, b.y), bulge: ax.bulge });
        }
        let (a, b) = shift_left(v2(s.a.0, s.a.1), v2(s.b.0, s.b.1), s.bulge, s.ecc);
        Ok(StripAxis::Arc { a: (a.x, a.y), b: (b.x, b.y), bulge: s.bulge })
    }

    pub fn add_raft(&mut self, s: &RaftSpec) -> Result<i64, Error> {
        check_raft(&self.db, s)?;
        self.op("footing_raft_add", |db| {
            let id = insert_raft(db, s, None)?;
            let inv = Inv::default()
                .delete("raft_vertex", "raft", id)
                .delete("raft", "id", id);
            Ok((id, id, inv))
        })
    }

    pub fn update_raft(&mut self, id: i64, s: &RaftSpec) -> Result<(), Error> {
        check_raft(&self.db, s)?;
        self.op("footing_raft_edit", |db| {
            let inv = Inv::default()
                .delete("raft_vertex", "raft", id)
                .restore(db, "raft", "id", id)?
                .restore(db, "raft_vertex", "raft", id)?;
            if db.execute("DELETE FROM raft WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such raft {id}"));
            }
            insert_raft(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn raft(&self, id: i64) -> Result<RaftSpec, Error> {
        raft_spec(&self.db, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColumnSpec, LayerSpec, WallSpec};

    fn near(a: f64, b: f64, what: &str) {
        assert!((a - b).abs() < 1e-6, "{what}: {a} vs {b}");
    }

    fn setup() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let l1 = m.add_level("L1", 3.0).unwrap();
        (m, l0, l1)
    }

    /// Bounding box of the view's solids (min, max).
    fn bbox(m: &Model, kind: &str) -> (V3, V3) {
        let big = ddc_geom::v3(f64::MAX, f64::MAX, f64::MAX);
        let small = ddc_geom::v3(f64::MIN, f64::MIN, f64::MIN);
        m.struct_meshes(None)
            .unwrap()
            .into_iter()
            .filter(|t| t.0 == kind)
            .flat_map(|t| t.2.positions)
            .fold((big, small), |(lo, hi), p| {
                (
                    ddc_geom::v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)),
                    ddc_geom::v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)),
                )
            })
    }

    fn column_at(m: &mut Model, l0: i64, l1: i64, at: (f64, f64), rotation: f64) -> i64 {
        let t = m
            .member_types()
            .unwrap()
            .into_iter()
            .find(|t| t.1 == "R 400x400")
            .map(|t| t.0)
            .unwrap_or_else(|| m.add_catalog_member_type("R 400x400").unwrap());
        m.add_column(&ColumnSpec {
            level: l0,
            top_level: Some(l1),
            member_type: t,
            structural: true,
            at,
            base_offset: 0.0,
            top_offset: 0.0,
            rotation,
            top_at: None,
        })
        .unwrap()
    }

    fn center(m: &Model) -> (f64, f64) {
        let (lo, hi) = bbox(m, "footing");
        ((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0)
    }

    #[test]
    fn footing_offset_turns_with_the_column() {
        let (mut m, l0, l1) = setup();
        let c = column_at(&mut m, l0, l1, (2.0, 0.0), std::f64::consts::FRAC_PI_2);
        let mut s = template(l0);
        s.column = Some(c);
        s.at = (1.0, 0.0);
        let id = m.add_footing(&s).unwrap();
        let (x, y) = center(&m);
        near(x, 2.0, "x");
        near(y, 1.0, "eccentricity along the turned column axis");
        assert_eq!(m.footing(id).unwrap().at, (1.0, 0.0));
    }

    #[test]
    fn footing_detaches_from_deleted_column_and_ignores_reused_id() {
        let (mut m, l0, l1) = setup();
        let c = column_at(&mut m, l0, l1, (3.0, 0.0), 0.3);
        let id = m.footings_under_columns(&[c], &template(l0)).unwrap()[0];
        let mut cs = m.column(c).unwrap();
        cs.at = (5.0, 2.0);
        cs.base_offset = -0.4;
        m.update_column(c, &cs).unwrap();
        let (lo0, hi0) = bbox(&m, "footing");
        m.delete_column(c).unwrap();
        let f = m.footing(id).unwrap();
        assert_eq!(f.column, None);
        near(f.at.0, 5.0, "kept x");
        near(f.at.1, 2.0, "kept y");
        near(f.rotation, 0.3, "kept turn");
        let (lo, hi) = bbox(&m, "footing");
        near(hi.z, hi0.z, "kept top");
        near(lo.x, lo0.x, "kept extent");
        let c2 = column_at(&mut m, l0, l1, (20.0, 0.0), 0.0);
        assert_eq!(c2, c, "id reused");
        assert_eq!(m.footing(id).unwrap().column, None);
        near(center(&m).0, 5.0, "not moved to the new column");
        m.undo().unwrap();
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.footing(id).unwrap().column, Some(c));
        near(center(&m).0, 3.0, "back under the column");
        assert!(m.check_integrity().unwrap().is_empty());
    }

    #[test]
    fn strip_fallback_follows_moved_wall_then_stays() {
        let (mut m, l0, _) = setup();
        let w = walls(&mut m, l0, &[((0.0, 0.0), (5.0, 0.0))]);
        let id = m
            .strip_footings_under_walls(&w, 0.6, 0.5, CONCRETE)
            .unwrap()[0];
        m.set_wall_axis(w[0], (0.0, 2.0), (5.0, 2.0)).unwrap();
        let s = m.strip_footing(id).unwrap();
        near(s.a.1, 2.0, "fallback follows");
        m.delete_wall(w[0]).unwrap();
        let s = m.strip_footing(id).unwrap();
        assert_eq!(s.wall, None);
        let (lo, hi) = bbox(&m, "strip_footing");
        near((lo.y + hi.y) / 2.0, 2.0, "stays at the last wall position");
        near(hi.z, 0.0, "top kept");
    }

    #[test]
    fn overlapping_strips_warn_and_count_once() {
        let (mut m, l0, _) = setup();
        let strip = |a, b, depth| StripFootingSpec {
            level: l0,
            wall: None,
            a,
            b,
            bulge: 0.0,
            width: 0.6,
            depth,
            ecc: 0.0,
            top_offset: 0.0,
            material: CONCRETE.into(),
        };
        let ids = m
            .add_strip_footings(&[
                strip((0.0, 0.0), (4.0, 0.0), 0.5),
                strip((2.0, -2.0), (2.0, 2.0), 0.8),
            ])
            .unwrap();
        let v = |i: usize| m.struct_quantities("strip_footing", ids[i]).unwrap().volume;
        near(v(0), 4.0 * 0.6 * 0.5, "first whole");
        near(v(1), 4.0 * 0.6 * 0.8 - 0.36 * 0.5, "overlap taken once");
        let issues = m.check_integrity().unwrap();
        assert!(issues
            .iter()
            .any(|i| i.id == ids[1] && i.problem.contains("overlaps strip footing")));
    }

    #[test]
    fn strip_lines_have_no_seams_and_section_shows_footings() {
        let (mut m, l0, _) = setup();
        m.add_strip_footings(&[StripFootingSpec {
            level: l0,
            wall: None,
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            bulge: 0.0,
            width: 0.6,
            depth: 0.5,
            ecc: 0.0,
            top_offset: 0.0,
            material: CONCRETE.into(),
        }])
        .unwrap();
        m.add_footing(&template(l0)).unwrap();
        let b = m.struct_build().unwrap();
        let p = b.parts.iter().find(|p| p.kind == "strip_footing").unwrap();
        // 4 + 4 horizontal edges and 4 vertical ones
        assert_eq!(p.lines.as_ref().unwrap().len(), 12);
        let v = m
            .section_view(
                &crate::SectionSpec {
                    kind: crate::ViewKind::Section,
                    name: "S".into(),
                    a: (-2.0, 0.0),
                    b: (6.0, 0.0),
                    depth: 5.0,
                    far_clip: true,
                },
                &[],
            )
            .unwrap();
        for t in ["iso_footing", "strip_footing"] {
            assert!(
                v.prims
                    .iter()
                    .any(|p| matches!(p, crate::ViewPrim::SectionCut { table, .. } if *table == t)),
                "{t} in section"
            );
        }
        // one build per state
        assert!(std::sync::Arc::ptr_eq(&b, &m.struct_build().unwrap()));
    }

    fn template(level: i64) -> FootingSpec {
        FootingSpec {
            level,
            column: None,
            at: (0.0, 0.0),
            rotation: 0.0,
            round: false,
            steps: vec![
                Step {
                    w: 1.8,
                    l: 1.8,
                    h: 0.3,
                },
                Step {
                    w: 1.0,
                    l: 1.0,
                    h: 0.3,
                },
            ],
            top_offset: 0.0,
            material: CONCRETE.into(),
        }
    }

    #[test]
    fn stepped_footing_follows_column_and_undo() {
        let (mut m, l0, l1) = setup();
        let t = m.add_catalog_member_type("R 400x400").unwrap();
        let c = m
            .add_column(&ColumnSpec {
                level: l0,
                top_level: Some(l1),
                member_type: t,
                structural: true,
                at: (3.0, 0.0),
                base_offset: -0.2,
                top_offset: 0.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
        let ids = m.footings_under_columns(&[c, c], &template(l0)).unwrap();
        assert_eq!(ids.len(), 1, "one footing per column");
        assert!(m
            .footings_under_columns(&[c], &template(l0))
            .unwrap()
            .is_empty());
        let q = m.struct_quantities("footing", ids[0]).unwrap();
        near(q.volume, 1.8 * 1.8 * 0.3 + 0.3, "stepped volume");
        near(q.weight.unwrap(), q.volume * 2400.0, "weight by density");
        let (lo, hi) = bbox(&m, "footing");
        near(hi.z, -0.2, "top at column base");
        near(lo.z, -0.8, "bottom");
        near((lo.x + hi.x) / 2.0, 3.0, "under the column");
        let mut cs = m.column(c).unwrap();
        cs.at = (5.0, 1.0);
        m.update_column(c, &cs).unwrap();
        let (lo, hi) = bbox(&m, "footing");
        near((lo.x + hi.x) / 2.0, 5.0, "follows x");
        near((lo.y + hi.y) / 2.0, 1.0, "follows y");
        m.undo().unwrap();
        let (lo, hi) = bbox(&m, "footing");
        near((lo.x + hi.x) / 2.0, 3.0, "back with the column");
        m.undo().unwrap();
        assert!(m.struct_ids("footing", None).unwrap().is_empty());
        assert!(m.struct_quantities("footing", ids[0]).is_err());
        assert!(m.check_integrity().unwrap().is_empty());
    }

    #[test]
    fn free_round_footing_and_edit_undo() {
        let (mut m, l0, _) = setup();
        let mut s = template(l0);
        s.round = true;
        s.steps = vec![Step {
            w: 1.2,
            l: 1.2,
            h: 0.6,
        }];
        s.at = (1.0, 2.0);
        s.top_offset = -0.5;
        let id = m.add_footing(&s).unwrap();
        let q = m.struct_quantities("footing", id).unwrap();
        let exact = std::f64::consts::PI * 0.36 * 0.6;
        assert!(
            (q.volume - exact).abs() < exact * 0.01,
            "{} vs {exact}",
            q.volume
        );
        let mut s2 = s.clone();
        s2.steps[0].h = 1.0;
        m.update_footing(id, &s2).unwrap();
        assert_eq!(m.footing(id).unwrap().steps[0].h, 1.0);
        m.undo().unwrap();
        assert_eq!(m.footing(id).unwrap(), s);
        let mut bad = s.clone();
        bad.steps.clear();
        assert!(m.add_footing(&bad).is_err());
    }

    fn walls(m: &mut Model, l0: i64, axes: &[((f64, f64), (f64, f64))]) -> Vec<i64> {
        let wt = m
            .add_wall_type(
                "W200",
                &[LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        axes.iter()
            .map(|&(a, b)| {
                m.add_wall(&WallSpec {
                    a,
                    b,
                    level: l0,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: wt,
                    offset0: None,
                })
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn strips_under_walls_join_at_l_and_t() {
        let (mut m, l0, _) = setup();
        let w = walls(
            &mut m,
            l0,
            &[
                ((0.0, 0.0), (6.0, 0.0)),
                ((6.0, 0.0), (6.0, 4.0)),
                ((3.0, 0.0), (3.0, -3.0)),
            ],
        );
        let ids = m
            .strip_footings_under_walls(&w, 0.6, 0.5, CONCRETE)
            .unwrap();
        assert_eq!(ids.len(), 3);
        assert!(m
            .strip_footings_under_walls(&w, 0.6, 0.5, CONCRETE)
            .unwrap()
            .is_empty());
        let vol: f64 = ids
            .iter()
            .map(|&i| m.struct_quantities("strip_footing", i).unwrap().volume)
            .sum();
        // L: 6.3·0.6 + 4.3·0.6 − 0.6² = 6.0 m²; T branch 2.7·0.6 = 1.62 m²; height 0.5
        near(vol, (6.0 + 1.62) * 0.5, "joined volume");
        let l = m.struct_quantities("strip_footing", ids[0]).unwrap().length;
        near(l, 6.0, "axis length");
        let (lo, hi) = bbox(&m, "strip_footing");
        near(hi.z, 0.0, "top at wall base");
        near(lo.z, -0.5, "bottom");
        m.undo().unwrap();
        assert!(m.struct_ids("strip_footing", None).unwrap().is_empty());
    }

    #[test]
    fn strip_follows_wall_then_keeps_axis_when_wall_is_gone() {
        let (mut m, l0, _) = setup();
        let w = walls(&mut m, l0, &[((0.0, 0.0), (5.0, 0.0))]);
        let id = m
            .strip_footings_under_walls(&w, 0.6, 0.5, CONCRETE)
            .unwrap()[0];
        let v = |m: &Model| m.struct_quantities("strip_footing", id).unwrap().volume;
        near(v(&m), 5.0 * 0.6 * 0.5, "under the wall");
        m.delete_wall(w[0]).unwrap();
        near(v(&m), 5.0 * 0.6 * 0.5, "own axis kept");
    }

    #[test]
    fn arc_strip_area_is_axis_length_times_width() {
        let (mut m, l0, _) = setup();
        let s = StripFootingSpec {
            level: l0,
            wall: None,
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            bulge: 0.5,
            width: 0.4,
            depth: 0.5,
            ecc: 0.0,
            top_offset: -0.1,
            material: CONCRETE.into(),
        };
        let id = m.add_strip_footings(std::slice::from_ref(&s)).unwrap()[0];
        let l = axis_length(v2(0.0, 0.0), v2(4.0, 0.0), 0.5);
        let q = m.struct_quantities("strip_footing", id).unwrap();
        near(q.length, l, "arc length");
        near(q.volume, l * 0.4 * 0.5, "arc volume");
        assert_eq!(m.strip_footing(id).unwrap(), s);
    }

    fn w200(m: &mut Model) -> i64 {
        m.add_wall_type(
            "W200 curve",
            &[LayerSpec {
                material: "Brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap()
    }

    #[test]
    fn strip_under_an_arc_wall_is_an_arc_band_and_undoes() {
        let (mut m, l0, _) = setup();
        let wt = w200(&mut m);
        let ws = [WallSpec {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            level: l0,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        }];
        let w = m.add_arc_walls(&ws, &[0.4]).unwrap();
        let id = m
            .strip_footings_under_walls(&w, 0.6, 0.5, CONCRETE)
            .unwrap()[0];
        let s = m.strip_footing(id).unwrap();
        assert_eq!(s.bulge, 0.4, "the band keeps the wall's arc, not its chord");
        let l = axis_length(v2(0.0, 0.0), v2(6.0, 0.0), 0.4);
        let q = m.struct_quantities("strip_footing", id).unwrap();
        let rel = |a: f64, b: f64| (a - b).abs() / b;
        assert!(rel(q.length, l) < 1e-6, "length {} vs {l}", q.length);
        assert!(rel(q.volume, 0.6 * 0.5 * l) < 1e-6, "volume {} vs {}", q.volume, 0.6 * 0.5 * l);
        // the band bows like the wall: its plan reaches the arc's sagitta, the chord does not
        let (lo, hi) = bbox(&m, "strip_footing");
        let sag = 0.4 * 3.0;
        assert!(hi.y - lo.y > sag + 0.5, "plan depth {} misses the arc", hi.y - lo.y);
        m.undo().unwrap();
        assert!(m.struct_ids("strip_footing", None).unwrap().is_empty());
    }

    #[test]
    fn strip_under_a_spline_wall_follows_the_spline() {
        use ddc_geom::wall_spline::{piece_dist, SplineAxis};
        let (mut m, l0, _) = setup();
        let wt = w200(&mut m);
        let sw = crate::SplineWallSpec::through(l0, wt, &[(0.0, 0.0), (4.0, 2.5), (9.0, -1.0), (13.0, 1.5)], 3.0)
            .unwrap();
        let sid = m.add_spline_wall(&sw).unwrap();
        let pieces = m.spline_wall_pieces(sid).unwrap();
        assert!(pieces.len() > 2, "{} pieces", pieces.len());
        // picking one piece gives the footing of the whole curve
        let ids = m
            .strip_footings_under_walls(&pieces[1..2], 0.6, 0.5, CONCRETE)
            .unwrap();
        assert_eq!(ids.len(), pieces.len());
        let specs: Vec<StripFootingSpec> = ids.iter().map(|&i| m.strip_footing(i).unwrap()).collect();
        assert!(specs.iter().all(|s| s.bulge != 0.0), "pieces are arcs, not chords");
        let ax = SplineAxis::new(sw.axis.clone()).unwrap();
        let (t0, t1) = sw.axis.domain();
        for k in 0..=400 {
            let t = t0 + (t1 - t0) * k as f64 / 400.0;
            let p = ax.point(t);
            let d = specs
                .iter()
                .map(|s| piece_dist(p, (v2(s.a.0, s.a.1), v2(s.b.0, s.b.1), s.bulge)))
                .fold(f64::MAX, f64::min);
            assert!(d <= 1e-3, "t {t}: footing axis {d} m off the spline");
        }
        let qs: Vec<_> = ids.iter().map(|&i| m.struct_quantities("strip_footing", i).unwrap()).collect();
        let len: f64 = qs.iter().map(|q| q.length).sum();
        let vol: f64 = qs.iter().map(|q| q.volume).sum();
        let exact = ax.layer_area(-0.3, 0.3) * 0.5;
        assert!((len - ax.length()).abs() / ax.length() < 1e-3, "length {len} vs {}", ax.length());
        assert!((vol - exact).abs() / exact < 1e-3, "volume {vol} vs {exact}");
        m.undo().unwrap();
        assert!(m.struct_ids("strip_footing", None).unwrap().is_empty(), "one undo step");
    }

    #[test]
    fn strip_follows_a_reshaped_spline_wall_in_one_step() {
        use ddc_geom::wall_spline::{piece_dist, SplineAxis};
        let (mut m, l0, _) = setup();
        let wt = w200(&mut m);
        let sw = crate::SplineWallSpec::through(l0, wt, &[(0.0, 0.0), (4.0, 2.5), (9.0, -1.0)], 3.0)
            .unwrap();
        let sid = m.add_spline_wall(&sw).unwrap();
        let pieces = m.spline_wall_pieces(sid).unwrap();
        m.strip_footings_under_walls(&pieces[..1], 0.6, 0.5, CONCRETE).unwrap();
        // every piece carries exactly one footing whose axis lies on the spline
        let check = |m: &Model, sw: &crate::SplineWallSpec| -> Vec<StripFootingSpec> {
            let pieces = m.spline_wall_pieces(sid).unwrap();
            let ids = m.struct_ids("strip_footing", None).unwrap();
            let specs: Vec<StripFootingSpec> = ids.iter().map(|&i| m.strip_footing(i).unwrap()).collect();
            assert_eq!(specs.len(), pieces.len(), "one footing per piece");
            for p in &pieces {
                assert_eq!(specs.iter().filter(|s| s.wall == Some(*p)).count(), 1, "piece {p}");
            }
            let ax = SplineAxis::new(sw.axis.clone()).unwrap();
            let (t0, t1) = sw.axis.domain();
            for k in 0..=400 {
                let p = ax.point(t0 + (t1 - t0) * k as f64 / 400.0);
                let d = specs
                    .iter()
                    .map(|s| piece_dist(p, (v2(s.a.0, s.a.1), v2(s.b.0, s.b.1), s.bulge)))
                    .fold(f64::MAX, f64::min);
                assert!(d <= 1e-3, "footing axis {d} m off the spline");
            }
            let len: f64 = ids.iter().map(|&i| m.struct_quantities("strip_footing", i).unwrap().length).sum();
            assert!((len - ax.length()).abs() / ax.length() < 1e-3, "length {len} vs {}", ax.length());
            specs
        };
        let before = check(&m, &sw);
        // a longer, wavier axis needs more pieces; a short one fewer
        let long = crate::SplineWallSpec::through(
            l0,
            wt,
            &[(0.0, 0.0), (4.0, 2.5), (9.0, -1.0), (13.0, 1.5), (17.0, -2.0), (20.0, 1.0)],
            3.0,
        )
        .unwrap();
        m.update_spline_wall(sid, &long).unwrap();
        assert!(m.spline_wall_pieces(sid).unwrap().len() > pieces.len());
        check(&m, &long);
        let short = crate::SplineWallSpec::through(l0, wt, &[(0.0, 0.0), (3.0, 0.8), (6.0, 0.0)], 3.0).unwrap();
        m.update_spline_wall(sid, &short).unwrap();
        check(&m, &short);
        m.undo().unwrap();
        check(&m, &long);
        m.undo().unwrap();
        assert_eq!(check(&m, &sw), before, "one undo step per reshape");
        // deleting the wall keeps the footings in place, unhosted, in the same step
        m.delete_spline_wall(sid).unwrap();
        let ids = m.struct_ids("strip_footing", None).unwrap();
        assert_eq!(ids.len(), before.len());
        assert!(ids.iter().all(|&i| m.strip_footing(i).unwrap().wall.is_none()));
        m.undo().unwrap();
        assert_eq!(check(&m, &sw), before);
        assert!(m.check_integrity().unwrap().is_empty());
    }

    #[test]
    fn raft_thickened_edge_edit_delete_undo() {
        let (mut m, l0, _) = setup();
        let s = RaftSpec {
            level: l0,
            ring: vec![(0.0, 0.0), (10.0, 0.0), (10.0, 8.0), (0.0, 8.0)],
            thickness: 0.3,
            edge_width: 0.6,
            edge_depth: 0.3,
            top_offset: 0.0,
            material: CONCRETE.into(),
        };
        let id = m.add_raft(&s).unwrap();
        let v = |m: &Model| m.struct_quantities("raft", id).unwrap().volume;
        let full = 80.0 * 0.3 + (80.0 - 8.8 * 6.8) * 0.3;
        near(v(&m), full, "slab + edge rib");
        let mut flat = s.clone();
        flat.edge_depth = 0.0;
        flat.ring.push((-1.0, 4.0));
        m.update_raft(id, &flat).unwrap();
        assert_eq!(m.raft(id).unwrap().ring.len(), 5);
        m.undo().unwrap();
        assert_eq!(m.raft(id).unwrap(), s);
        near(v(&m), full, "restored");
        m.delete_struct("raft", id).unwrap();
        assert!(m.raft(id).is_err());
        m.undo().unwrap();
        assert_eq!(m.raft(id).unwrap(), s);
        let (lo, _) = bbox(&m, "raft");
        near(lo.z, -0.6, "rib bottom");
        assert!(m.check_integrity().unwrap().is_empty());
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
    }

    /// A storage tank's ring: four quarter arcs on their own axis, no wall to stand on, close
    /// into one ring of the full width and depth below the level.
    #[test]
    fn a_ring_of_arc_strips_without_a_wall_builds_whole() {
        let (mut m, l0, _) = setup();
        let (r, w, d) = (6.0, 1.0, 1.2);
        let q = std::f64::consts::FRAC_PI_2;
        let at = |k: f64| (r * (k * q).cos(), r * (k * q).sin());
        let ring: Vec<StripFootingSpec> = (0..4)
            .map(|k| StripFootingSpec {
                level: l0,
                wall: None,
                a: at(k as f64),
                b: at(k as f64 + 1.0),
                bulge: (std::f64::consts::PI / 8.0).tan(),
                width: w,
                depth: d,
                ecc: 0.0,
                top_offset: 0.0,
                material: CONCRETE.into(),
            })
            .collect();
        let ids = m.add_strip_footings(&ring).unwrap();
        assert_eq!(ids.len(), 4);
        let b = m.struct_build().unwrap();
        let parts: Vec<_> = b.parts.iter().filter(|p| p.kind == "strip_footing").collect();
        assert_eq!(parts.len(), 4);
        let vol: f64 = parts.iter().map(|p| p.volume).sum();
        let want = 2.0 * std::f64::consts::PI * r * w * d;
        assert!((vol - want).abs() < 0.02 * want, "{vol} vs {want}");
        let (lo, hi) = bbox(&m, "strip_footing");
        near(hi.x, r + w / 2.0, "outer edge");
        near(lo.y, -(r + w / 2.0), "outer edge");
        near(lo.z, -d, "bottom");
        near(hi.z, 0.0, "top");
        m.update_strip_footing(ids[0], &StripFootingSpec { width: 1.5, ..ring[0].clone() }).unwrap();
        assert!(m.check_integrity().unwrap().is_empty());
    }
}
