// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Roofs and edges (schema step 27): extrusion with arcs and offsets, joining to another
//! roof, outline-roof presets, skylights (they cut the roof), eave soffit, profile
//! elements along edges (fascia, gutter, slab edge, wall moulding/cornice, reveal, which is
//! subtracted from the wall), and tapered insulation of a flat roof toward drains.
//! Section profiles are JSON `[[x, y, bulge], ...]`; a `sweep_profile` row with the same name
//! overrides the built-in profile.

use ddc_geom::cut::Cutter;
use ddc_geom::math::{Tol, V2, V3};
use ddc_geom::mesh::Mesh;
use ddc_geom::roof::{LineKind, Roof};
use ddc_geom::roof_shape::{self, ArcExtrusion, Opening, Preset};
use ddc_geom::sweep::{self, Path, SectionProfile, SweepKind, SweepQty};
use ddc_geom::taper::{Taper, TaperQty};
use ddc_geom::{v2, v3};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::roofs::{self, RoofKind, RoofSpec};
use crate::{Error, Issue, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS roof_ext(roof INTEGER PRIMARY KEY, bulges TEXT NOT NULL DEFAULT '[]',
    w0 REAL NOT NULL DEFAULT 0, join_roof INTEGER);
CREATE TABLE IF NOT EXISTS sweep_profile(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL, pts TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS sweep(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, level INTEGER NOT NULL,
    profile TEXT NOT NULL, host_kind TEXT NOT NULL, hosts TEXT NOT NULL, edges TEXT,
    side INTEGER NOT NULL DEFAULT -1, z REAL NOT NULL DEFAULT 0, dx REAL NOT NULL DEFAULT 0,
    dy REAL NOT NULL DEFAULT 0, material TEXT);
CREATE TABLE IF NOT EXISTS skylight(id INTEGER PRIMARY KEY, roof INTEGER NOT NULL, x REAL NOT NULL,
    y REAL NOT NULL, width REAL NOT NULL, length REAL NOT NULL, curb REAL NOT NULL);
CREATE TABLE IF NOT EXISTS soffit(id INTEGER PRIMARY KEY, roof INTEGER NOT NULL,
    drop_h REAL NOT NULL DEFAULT 0, thickness REAL NOT NULL, material TEXT);
CREATE TABLE IF NOT EXISTS roof_taper(id INTEGER PRIMARY KEY, level INTEGER NOT NULL, slab INTEGER,
    ring TEXT NOT NULL, offset REAL NOT NULL, slope REAL NOT NULL, tmin REAL NOT NULL,
    ax REAL NOT NULL DEFAULT 1, ay REAL NOT NULL DEFAULT 0, material TEXT);
CREATE TABLE IF NOT EXISTS roof_drain(id INTEGER PRIMARY KEY, taper INTEGER NOT NULL, x REAL NOT NULL,
    y REAL NOT NULL);
";

/// Step 27 tables (CREATE IF NOT EXISTS, so migration order does not matter).
pub(crate) fn ensure(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

/// Limit of |bulge| for vault arcs (rise <= 0.4 of the span).
const MAX_BULGE: f64 = 0.8;

fn invalid<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

fn geom<E: std::fmt::Debug>(what: &'static str) -> impl Fn(E) -> Error {
    move |e| Error::Geom(format!("{what}: {e:?}"))
}

fn ids(db: &Connection, sql: &str, arg: i64) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare(sql)?;
    let r = st.query_map([arg], |r| r.get(0))?;
    Ok(r.collect::<Result<_, _>>()?)
}

fn exists(db: &Connection, table: &str, id: i64) -> Result<bool, Error> {
    Ok(db.query_row(
        &format!("SELECT count(*) FROM {table} WHERE id=?1"),
        [id],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

fn elevation(db: &Connection, level: i64) -> Result<f64, Error> {
    db.query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("no level {level}")))
}

fn thickness(db: &Connection, stype: i64) -> Result<f64, Error> {
    Ok(db.query_row(
        "SELECT COALESCE(SUM(thickness), 0) FROM slab_type_layer WHERE stype=?1",
        [stype],
        |r| r.get(0),
    )?)
}

fn merge(into: &mut Mesh, m: &Mesh) {
    let base = into.positions.len() as u32;
    let f0 = into
        .face_of
        .iter()
        .copied()
        .max()
        .map_or(0, |f| f.saturating_add(1));
    // face numbers `m` run consecutively from f0: sweep ends can have large numbers
    let mut fs: Vec<u32> = m.face_of.clone();
    fs.sort_unstable();
    fs.dedup();
    into.positions.extend_from_slice(&m.positions);
    into.triangles
        .extend(m.triangles.iter().map(|t| t.map(|i| i + base)));
    into.face_of.extend(
        m.face_of
            .iter()
            .map(|f| f0.saturating_add(fs.binary_search(f).unwrap_or(0) as u32)),
    );
}

// ---------------------------------------------------------------- roof shape

type Ext = (Vec<f64>, f64, Option<i64>);

fn ext(db: &Connection, id: i64) -> Result<Option<Ext>, Error> {
    let row: Option<(String, f64, Option<i64>)> = db
        .query_row(
            "SELECT bulges, w0, join_roof FROM roof_ext WHERE roof=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    row.map(|(b, w0, j)| Ok((serde_json::from_str(&b)?, w0, j)))
        .transpose()
}

/// Roof shape and warning: a join that cannot be built (the target was edited/deleted) is
/// dropped, so the roof is built without it and editing the target is not blocked.
fn own(
    db: &Connection,
    id: i64,
    s: &RoofSpec,
    follow_join: bool,
) -> Result<(Roof, Option<String>), Error> {
    let RoofKind::Extrusion {
        a,
        b,
        profile,
        depth,
    } = &s.kind
    else {
        return Ok((roofs::geometry(db, s)?, None));
    };
    let Some((bulges, w0, join)) = ext(db, id)? else {
        return Ok((roofs::geometry(db, s)?, None));
    };
    let ex = ArcExtrusion {
        a: v2(a.0, a.1),
        b: v2(b.0, b.1),
        profile: profile.clone(),
        bulges,
        w0,
        depth: *depth,
    };
    let z = elevation(db, s.level)? + s.offset;
    let plain = || ex.build(z).map_err(geom("roof"));
    let Some(t) = join.filter(|&t| follow_join && t != id) else {
        return Ok((plain()?, None));
    };
    if !exists(db, "roof", t)? {
        return Ok((plain()?, Some(format!("join target roof {t} is missing"))));
    }
    let target = roofs::load(db, t).and_then(|ts| own(db, t, &ts, false));
    match target.map(|tr| roof_shape::join(&ex, z, &tr.0)) {
        Ok(Ok(r)) => Ok((r, None)),
        Ok(Err(e)) => Ok((plain()?, Some(format!("join to roof {t} failed: {e:?}")))),
        Err(e) => Ok((plain()?, Some(format!("join target roof {t}: {e}")))),
    }
}

/// Roof shape without skylight cutouts (walls attach to it).
pub(crate) fn shape(db: &Connection, id: i64, s: &RoofSpec) -> Result<Roof, Error> {
    Ok(own(db, id, s, true)?.0)
}

/// Like `shape`, but a failed join is an error (when editing the join itself).
fn shape_strict(db: &Connection, id: i64, s: &RoofSpec) -> Result<Roof, Error> {
    match own(db, id, s, true)? {
        (_, Some(w)) => invalid(w),
        (r, None) => Ok(r),
    }
}

fn openings(db: &Connection, roof: i64) -> Result<Vec<(i64, Opening, f64)>, Error> {
    let mut st = db.prepare_cached(
        "SELECT id, x, y, width, length, curb FROM skylight WHERE roof=?1 ORDER BY id",
    )?;
    let r = st.query_map([roof], |r| {
        Ok((
            r.get(0)?,
            Opening {
                center: v2(r.get(1)?, r.get(2)?),
                width: r.get(3)?,
                length: r.get(4)?,
            },
            r.get(5)?,
        ))
    })?;
    Ok(r.collect::<Result<_, _>>()?)
}

/// A roof with skylight cutouts; skylights that fit are (id, corners, normal, box), those that no longer
/// fit after a roof edit are (id, reason): they do not cut the roof and are not drawn.
pub(crate) struct Lit {
    pub roof: Roof,
    pub cuts: Vec<(i64, [V3; 4], V3, f64)>,
    pub skipped: Vec<(i64, String)>,
}

fn lit(db: &Connection, id: i64, s: &RoofSpec) -> Result<Lit, Error> {
    let mut roof = shape(db, id, s)?;
    let (mut cuts, mut skipped) = (vec![], vec![]);
    for (k, o, curb) in openings(db, id)? {
        match roof_shape::cut_opening(&roof, &o) {
            Ok(c) => {
                cuts.push((k, c.corners, c.normal, curb));
                roof = c.roof;
            }
            Err(e) => skipped.push((k, format!("{e:?}"))),
        }
    }
    Ok(Lit {
        roof,
        cuts,
        skipped,
    })
}

/// Shape with skylight cutouts: quantities, body, views.
pub(crate) fn cut(db: &Connection, id: i64, s: &RoofSpec) -> Result<Roof, Error> {
    Ok(lit(db, id, s)?.roof)
}

/// Deleting a roof: skylights, soffits, profiles along its edges, other roofs' joins to it.
pub(crate) fn drop_roof(db: &Connection, mut inv: Inv, id: i64) -> Result<Inv, Error> {
    for t in ["roof_ext", "skylight", "soffit"] {
        inv = inv.restore(db, t, "roof", id)?;
        db.execute(&format!("DELETE FROM {t} WHERE roof=?1"), [id])?;
    }
    let deps = ids(
        db,
        "SELECT roof FROM roof_ext WHERE join_roof=?1 ORDER BY roof",
        id,
    )?;
    for &r in &deps {
        inv = inv
            .delete("roof_ext", "roof", r)
            .restore(db, "roof_ext", "roof", r)?;
        db.execute("UPDATE roof_ext SET join_roof=NULL WHERE roof=?1", [r])?;
    }
    for s in hosted(db, "roof", id)? {
        inv = inv.restore(db, "sweep", "id", s)?;
        db.execute("DELETE FROM sweep WHERE id=?1", [s])?;
    }
    // wing roofs are now without a join: walls under them follow the new shape
    for r in deps {
        inv = roofs::reattach_soft(db, r, inv)?;
    }
    Ok(inv)
}

/// Walls under roofs joined to `target`, per their new shape (after the target is edited).
pub(crate) fn reattach_dependents(
    db: &Connection,
    mut inv: Inv,
    target: i64,
) -> Result<Inv, Error> {
    for r in ids(
        db,
        "SELECT roof FROM roof_ext WHERE join_roof=?1 ORDER BY roof",
        target,
    )? {
        inv = roofs::reattach_soft(db, r, inv)?;
    }
    Ok(inv)
}

/// Deleting hosts `gone` (walls or slabs): profile elements lose them (with no hosts left they are
/// deleted), a taper on a slab is deleted with its drains. The host number is freed, so a
/// new element with the same number does not inherit another's cornice/reveal.
pub(crate) fn drop_hosts(
    db: &Connection,
    mut inv: Inv,
    kind: &str,
    gone: &[i64],
) -> Result<Inv, Error> {
    if gone.is_empty() {
        return Ok(inv);
    }
    let rows: Vec<(i64, String)> = {
        let mut st = db.prepare("SELECT id, hosts FROM sweep WHERE host_kind=?1 ORDER BY id")?;
        let r = st.query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))?;
        r.collect::<Result<_, _>>()?
    };
    for (id, h) in rows {
        let Ok(hs) = serde_json::from_str::<Vec<i64>>(&h) else {
            continue;
        };
        if !hs.iter().any(|x| gone.contains(x)) {
            continue;
        }
        inv = inv.restore(db, "sweep", "id", id)?;
        let keep: Vec<i64> = hs.into_iter().filter(|x| !gone.contains(x)).collect();
        if keep.is_empty() || kind != "wall" {
            db.execute("DELETE FROM sweep WHERE id=?1", [id])?;
        } else {
            db.execute(
                "UPDATE sweep SET hosts=?2 WHERE id=?1",
                params![id, serde_json::to_string(&keep)?],
            )?;
        }
    }
    if kind == "slab" {
        for &sl in gone {
            for t in ids(
                db,
                "SELECT id FROM roof_taper WHERE slab=?1 ORDER BY id",
                sl,
            )? {
                inv = inv.restore(db, "roof_taper", "id", t)?.restore(
                    db,
                    "roof_drain",
                    "taper",
                    t,
                )?;
                db.execute("DELETE FROM roof_drain WHERE taper=?1", [t])?;
                db.execute("DELETE FROM roof_taper WHERE id=?1", [t])?;
            }
        }
    }
    Ok(inv)
}

/// Skylights follow a roof moved/rotated as a whole (rigid motion of the outline).
pub(crate) fn follow_roof(
    db: &Connection,
    mut inv: Inv,
    id: i64,
    old: &RoofSpec,
    new: &RoofSpec,
) -> Result<Inv, Error> {
    let pts = |s: &RoofSpec| -> Vec<(f64, f64)> {
        match &s.kind {
            RoofKind::Footprint { ring, .. } => ring.clone(),
            RoofKind::Extrusion { a, b, .. } => vec![*a, *b],
            RoofKind::Arrow { ring, .. } => ring.clone(),
        }
    };
    let (p, q) = (pts(old), pts(new));
    let Some(xf) = rigid(&p, &q) else {
        return Ok(inv);
    };
    for (k, o, _) in openings(db, id)? {
        let c = xf(o.center);
        inv = inv.restore(db, "skylight", "id", k)?;
        db.execute(
            "UPDATE skylight SET x=?2, y=?3 WHERE id=?1",
            params![k, c.x, c.y],
        )?;
    }
    Ok(inv)
}

/// A rotation + translation (no mirror) mapping `p` to `q`; identity and non-rigid give None.
fn rigid(p: &[(f64, f64)], q: &[(f64, f64)]) -> Option<impl Fn(V2) -> V2> {
    if p.len() != q.len() || p.len() < 2 {
        return None;
    }
    let v = |a: (f64, f64)| v2(a.0, a.1);
    let (p0, p1, q0, q1) = (v(p[0]), v(p[1]), v(q[0]), v(q[1]));
    let (dp, dq) = (p1 - p0, q1 - q0);
    let l = dp.len();
    if l < 1e-9 || (dq.len() - l).abs() > 1e-6 {
        return None;
    }
    let (cs, sn) = (dp.dot(dq) / (l * l), dp.cross(dq) / (l * l));
    let f = move |x: V2| {
        let d = x - p0;
        q0 + v2(cs * d.x - sn * d.y, sn * d.x + cs * d.y)
    };
    let moved = p.iter().zip(q).any(|(a, b)| (v(*a) - v(*b)).len() > 1e-9);
    let ok = p
        .iter()
        .zip(q)
        .all(|(a, b)| (f(v(*a)) - v(*b)).len() < 1e-6);
    (moved && ok).then_some(f)
}

fn hosted(db: &Connection, kind: &str, host: i64) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare("SELECT id, hosts FROM sweep WHERE host_kind=?1 ORDER BY id")?;
    let rows: Vec<(i64, String)> = st
        .query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|(_, h)| serde_json::from_str::<Vec<i64>>(h).is_ok_and(|v| v.contains(&host)))
        .map(|r| r.0)
        .collect())
}

// ---------------------------------------------------------------- profile elements

#[derive(Clone, Debug, PartialEq)]
pub enum SweepHost {
    /// Roof eaves; `rakes` also includes gable overhangs.
    Roof { roof: i64, rakes: bool },
    /// Outer slab outline at the top.
    Slab(i64),
    /// A wall chain (neighbours share an end); the path follows a wall face.
    Walls(Vec<i64>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SweepSpec {
    pub kind: SweepKind,
    /// Profile name (built-in or from `sweep_profile`).
    pub profile: String,
    pub host: SweepHost,
    /// Wall face: -1 right along the chain, +1 left (roof/slab always outward).
    pub side: i8,
    /// Path elevation: above the wall bottom; for a roof/slab, a height shift.
    pub z: f64,
    /// Section shift: outward and upward.
    pub dx: f64,
    pub dy: f64,
    pub material: Option<String>,
}

fn host_text(h: &SweepHost) -> (&'static str, String, Option<&'static str>) {
    match h {
        SweepHost::Roof { roof, rakes } => (
            "roof",
            format!("[{roof}]"),
            Some(if *rakes { "eave,rake" } else { "eave" }),
        ),
        SweepHost::Slab(s) => ("slab", format!("[{s}]"), None),
        SweepHost::Walls(w) => (
            "wall",
            serde_json::to_string(w).unwrap_or_else(|_| "[]".into()),
            None,
        ),
    }
}

fn load_sweep(db: &Connection, id: i64) -> Result<SweepSpec, Error> {
    type Row = (
        String,
        String,
        String,
        String,
        Option<String>,
        i64,
        f64,
        f64,
        f64,
        Option<String>,
    );
    let r: Row = db
        .query_row(
            "SELECT kind, profile, host_kind, hosts, edges, side, z, dx, dy, material FROM sweep WHERE id=?1",
            [id],
            |r| {
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
            },
        )
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no such sweep {id}")))?;
    let kind =
        SweepKind::parse(&r.0).ok_or_else(|| Error::Invalid(format!("sweep kind {}", r.0)))?;
    let hosts: Vec<i64> = serde_json::from_str(&r.3)?;
    let first = match (r.2.as_str(), hosts.first()) {
        ("wall", _) => 0,
        (_, Some(&h)) => h,
        (k, None) => return invalid(format!("sweep {id}: no {k} host")),
    };
    let host = match r.2.as_str() {
        "roof" => SweepHost::Roof {
            roof: first,
            rakes: r.4.as_deref().is_some_and(|e| e.contains("rake")),
        },
        "slab" => SweepHost::Slab(first),
        _ => SweepHost::Walls(hosts),
    };
    Ok(SweepSpec {
        kind,
        profile: r.1,
        host,
        side: if r.5 > 0 { 1 } else { -1 },
        z: r.6,
        dx: r.7,
        dy: r.8,
        material: r.9,
    })
}

/// Profile by name: a `sweep_profile` row, otherwise the built-in one.
fn profile(db: &Connection, name: &str) -> Result<(SweepKind, SectionProfile), Error> {
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT kind, pts FROM sweep_profile WHERE name=?1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((k, pts)) = row {
        let kind =
            SweepKind::parse(&k).ok_or_else(|| Error::Invalid(format!("profile kind {k}")))?;
        let p = SectionProfile::from_json(&pts)
            .ok_or_else(|| Error::Invalid(format!("profile {name}: bad JSON")))?;
        return Ok((kind, p));
    }
    sweep::library()
        .into_iter()
        .find(|l| l.0 == name)
        .map(|l| (l.1, l.2))
        .ok_or_else(|| Error::Invalid(format!("no such profile {name}")))
}

fn roof_paths(db: &Connection, roof: i64, rakes: bool) -> Result<Vec<Path>, Error> {
    let s = roofs::load(db, roof)?;
    let r = cut(db, roof, &s)?;
    let mut segs = vec![];
    for l in &r.lines {
        if !(l.kind == LineKind::Eave || (rakes && l.kind == LineKind::Rake)) {
            continue;
        }
        let d = v2(l.b.x - l.a.x, l.b.y - l.a.y);
        let len = d.len();
        if len < 1e-9 {
            continue;
        }
        let m = v2(l.a.x + l.b.x, l.a.y + l.b.y) * 0.5;
        let left = v2(-d.y, d.x) * (1e-3 / len);
        // the roof is on the left, outward is right (the section is built to the right of the path)
        if r.top_at(m + left).is_none() && r.top_at(m - left).is_some() {
            segs.push((l.b, l.a));
        } else {
            segs.push((l.a, l.b));
        }
    }
    Ok(sweep::chain(&segs, 1e-6)
        .into_iter()
        .map(|(pts, closed)| {
            if closed {
                Path::closed(pts)
            } else {
                Path::open(pts)
            }
        })
        .collect())
}

fn slab_path(db: &Connection, slab: i64) -> Result<Path, Error> {
    let (level, offset): (i64, f64) = db
        .query_row("SELECT level, offset FROM slab WHERE id=?1", [slab], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no such slab {slab}")))?;
    let z = elevation(db, level)? + offset;
    let ring = crate::slabs::rings(db, slab)?
        .into_iter()
        .next()
        .ok_or_else(|| Error::Invalid(format!("slab {slab} has no outline")))?;
    let mut pts: Vec<V2> = ring.iter().map(|p| v2(p.0, p.1)).collect();
    if ddc_geom::poly::area(&pts) < 0.0 {
        pts.reverse();
    }
    Ok(Path::closed(
        pts.into_iter().map(|p| v3(p.x, p.y, z)).collect(),
    ))
}

/// Wall face along the chain: start, end, bulge, bottom elevation; for an arc, the centre and radius
/// of the offset circle.
struct Face {
    p: V2,
    q: V2,
    bulge: f64,
    base: f64,
    circle: Option<(V2, f64)>,
}

/// Tolerance for the shared end of adjacent chain walls.
const CHAIN_TOL: f64 = 1e-3;

struct WallRow {
    a: V2,
    b: V2,
    base: f64,
    offset0: Option<f64>,
    bulge: f64,
    t: f64,
}

/// Walls ordered into a connected chain by shared ends: (wall, whether along a->b), whether closed.
/// The direction follows the first selected wall. A gap or a branch is an error.
fn order_chain(rows: &[WallRow]) -> Result<(Vec<(usize, bool)>, bool), Error> {
    let n = rows.len();
    if n == 0 {
        return Ok((vec![], false));
    }
    let near = |x: V2, y: V2| (x - y).len() <= CHAIN_TOL;
    let touches = |i: usize, p: V2| near(rows[i].a, p) || near(rows[i].b, p);
    // the start is a free end (shared with no other wall); for a loop, the first wall
    let free = |i: usize, p: V2| !(0..n).any(|j| j != i && touches(j, p));
    let (start, fwd) = (0..n)
        .find_map(|i| {
            if free(i, rows[i].a) {
                Some((i, true))
            } else if free(i, rows[i].b) {
                Some((i, false))
            } else {
                None
            }
        })
        .unwrap_or((0, true));
    let end = |(i, f): (usize, bool)| if f { rows[i].b } else { rows[i].a };
    let mut used = vec![false; n];
    let mut out = vec![(start, fwd)];
    used[start] = true;
    while out.len() < n {
        let e = end(out[out.len() - 1]);
        let Some(j) = (0..n).find(|&j| !used[j] && touches(j, e)) else {
            return invalid(format!(
                "sweep: walls are not one connected chain (gap at {:.3}, {:.3})",
                e.x, e.y
            ));
        };
        used[j] = true;
        out.push((j, near(rows[j].a, e)));
    }
    let first = if fwd { rows[start].a } else { rows[start].b };
    let arcs = rows.iter().any(|r| r.bulge.abs() > 1e-9);
    let closed = (n > 2 || (n == 2 && arcs)) && near(end(out[n - 1]), first);
    // direction follows the first selected wall
    if let Some(k) = out.iter().position(|o| o.0 == 0) {
        if !out[k].1 {
            out.reverse();
            for o in &mut out {
                o.1 = !o.1;
            }
        }
    }
    Ok((out, closed))
}

/// Intersection point of the offset curves of two adjacent faces nearest to `guess`; with no
/// intersection (parallel, circles apart) the midpoint of the ends.
fn corner(f0: &Face, f1: &Face, guess: V2) -> V2 {
    let line = |f: &Face| (f.p, f.q - f.p);
    let pick = |c: &[V2]| {
        c.iter()
            .copied()
            .filter(|p| p.x.is_finite() && p.y.is_finite())
            .min_by(|x, y| (*x - guess).len().total_cmp(&(*y - guess).len()))
    };
    let lc = |(p, d): (V2, V2), (c, r): (V2, f64)| -> Vec<V2> {
        let dd = d.dot(d);
        if dd < 1e-24 {
            return vec![];
        }
        let w = p - c;
        let (bq, cq) = (w.dot(d) / dd, (w.dot(w) - r * r) / dd);
        let disc = bq * bq - cq;
        if disc < 0.0 {
            return vec![];
        }
        let s = disc.sqrt();
        vec![p + d * (-bq - s), p + d * (-bq + s)]
    };
    let got = match (f0.circle, f1.circle) {
        (None, None) => {
            let ((p0, d0), (p1, d1)) = (line(f0), line(f1));
            let den = d0.cross(d1);
            if den.abs() > 1e-9 * d0.len() * d1.len() {
                Some(p0 + d0 * ((p1 - p0).cross(d1) / den))
            } else {
                None
            }
        }
        (None, Some(c)) => pick(&lc(line(f0), c)),
        (Some(c), None) => pick(&lc(line(f1), c)),
        (Some((c0, r0)), Some((c1, r1))) => {
            let d = c1 - c0;
            let l = d.len();
            if l < 1e-12 || l > r0 + r1 || l < (r0 - r1).abs() {
                None
            } else {
                let x = (l * l + r0 * r0 - r1 * r1) / (2.0 * l);
                let h = (r0 * r0 - x * x).max(0.0).sqrt();
                let (u, v) = (d * (1.0 / l), v2(-d.y, d.x) * (1.0 / l));
                pick(&[c0 + u * x + v * h, c0 + u * x - v * h])
            }
        }
    };
    // a sharp angle without a cut gives a far point: beyond 4 thicknesses, the midpoint
    let lim = 4.0 * (f0.q - f1.p).len().max(0.5);
    got.filter(|p| (*p - guess).len() <= lim)
        .unwrap_or((f0.q + f1.p) * 0.5)
}

/// Bulge of a circle arc centred at `c` from p to q on the same side as `old`.
fn rebulge(c: V2, p: V2, q: V2, old: f64) -> f64 {
    let (ap, aq) = ((p - c).y.atan2((p - c).x), (q - c).y.atan2((q - c).x));
    let want = 4.0 * old.atan();
    let (pi, tau) = (std::f64::consts::PI, 2.0 * std::f64::consts::PI);
    let mut d = aq - ap;
    while d - want > pi {
        d -= tau;
    }
    while want - d > pi {
        d += tau;
    }
    (d / 4.0).tan()
}

fn wall_faces(db: &Connection, walls: &[i64], side: i8) -> Result<(Vec<Face>, bool), Error> {
    type Row = (f64, f64, f64, f64, f64, Option<f64>, f64, i64);
    let mut rows = vec![];
    for &w in walls {
        let r: Option<Row> = db
            .query_row(
                "SELECT w.ax, w.ay, w.bx, w.by, l.elevation + w.base_offset, w.offset0, \
                 w.bulge, w.wtype FROM wall w JOIN level l ON l.id=w.level WHERE w.id=?1",
                [w],
                |r| {
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
                },
            )
            .optional()?;
        // a deleted wall drops out of the chain
        let Some(r) = r else { continue };
        let t: f64 = db.query_row(
            "SELECT COALESCE(SUM(thickness), 0) FROM wall_type_layer WHERE wtype=?1",
            [r.7],
            |q| q.get(0),
        )?;
        rows.push(WallRow {
            a: v2(r.0, r.1),
            b: v2(r.2, r.3),
            base: r.4,
            offset0: r.5,
            bulge: r.6,
            t,
        });
    }
    let (order, closed) = order_chain(&rows)?;
    let mut out: Vec<Face> = vec![];
    for &(i, fwd) in &order {
        let r = &rows[i];
        let lo = r.offset0.unwrap_or(-r.t / 2.0);
        let hi = lo + r.t;
        let (p, q, bulge) = if fwd {
            (r.a, r.b, r.bulge)
        } else {
            (r.b, r.a, -r.bulge)
        };
        // face offset to the left along the direction of travel
        let o = match (side > 0, fwd) {
            (false, true) => lo,
            (false, false) => -hi,
            (true, true) => hi,
            (true, false) => -lo,
        };
        let face = if bulge.abs() > 1e-9 {
            let arc = ddc_geom::arc::ArcAxis::new(p, q, bulge, Tol::default())
                .map_err(geom("wall arc"))?;
            // left of a counter-clockwise arc's direction is toward the centre
            let rr = arc.radius - o * arc.sweep.signum();
            if rr <= 1e-6 {
                return invalid("sweep: offset exceeds wall arc radius");
            }
            let at = |x: V2| arc.center + (x - arc.center) * (rr / arc.radius);
            Face {
                p: at(p),
                q: at(q),
                bulge,
                base: r.base,
                circle: Some((arc.center, rr)),
            }
        } else {
            let d = (q - p) * (1.0 / (q - p).len().max(1e-12));
            let off = |c: V2| c + v2(-d.y, d.x) * o;
            Face {
                p: off(p),
                q: off(q),
                bulge,
                base: r.base,
                circle: None,
            }
        };
        out.push(face);
    }
    // corners: intersection of the offset curves of adjacent faces (for a loop, also last with first)
    let n = out.len();
    let mut pairs: Vec<(usize, usize)> = (1..n).map(|i| (i - 1, i)).collect();
    if closed {
        pairs.push((n - 1, 0));
    }
    for (i, j) in pairs {
        let guess = (out[i].q + out[j].p) * 0.5;
        let c = corner(&out[i], &out[j], guess);
        out[i].q = c;
        out[j].p = c;
    }
    for f in &mut out {
        if let Some((c, _)) = f.circle {
            f.bulge = rebulge(c, f.p, f.q, f.bulge);
        }
    }
    Ok((out, closed))
}

fn wall_path(db: &Connection, walls: &[i64], side: i8, z: f64) -> Result<Vec<Path>, Error> {
    let (f, closed) = wall_faces(db, walls, side)?;
    let Some(last) = f.last() else {
        return Ok(vec![]);
    };
    // elevation follows the bottom of each wall
    let mut pts: Vec<V3> = f.iter().map(|x| v3(x.p.x, x.p.y, x.base + z)).collect();
    if !closed {
        pts.push(v3(last.q.x, last.q.y, last.base + z));
    }
    let mut path = Path {
        pts,
        bulges: f.iter().map(|x| x.bulge).collect(),
        closed,
    };
    if side > 0 {
        path = path.reversed();
    }
    Ok(vec![path])
}

/// Top elevation of the chain's first wall above its bottom (for a "by top" cornice).
fn wall_height(db: &Connection, wall: i64) -> Result<f64, Error> {
    db.query_row("SELECT height FROM wall WHERE id=?1", [wall], |r| r.get(0))
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no such wall {wall}")))
}

fn paths(db: &Connection, s: &SweepSpec) -> Result<Vec<Path>, Error> {
    let mut ps = match &s.host {
        SweepHost::Roof { roof, rakes } => roof_paths(db, *roof, *rakes)?,
        SweepHost::Slab(slab) => vec![slab_path(db, *slab)?],
        SweepHost::Walls(w) => return wall_path(db, w, s.side, s.z),
    };
    for p in &mut ps {
        for q in &mut p.pts {
            q.z += s.z;
        }
    }
    Ok(ps)
}

fn section(db: &Connection, s: &SweepSpec) -> Result<SectionProfile, Error> {
    let (_, p) = profile(db, &s.profile)?;
    Ok(p.shifted(s.dx, s.dy))
}

fn sweep_level(db: &Connection, h: &SweepHost) -> Result<i64, Error> {
    let (t, id) = match h {
        SweepHost::Roof { roof, .. } => ("roof", *roof),
        SweepHost::Slab(s) => ("slab", *s),
        SweepHost::Walls(w) => (
            "wall",
            *w.first()
                .ok_or_else(|| Error::Invalid("sweep: no walls".into()))?,
        ),
    };
    db.query_row(&format!("SELECT level FROM {t} WHERE id=?1"), [id], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("sweep: no such {t} {id}")))
}

fn sweep_body(db: &Connection, s: &SweepSpec) -> Result<Mesh, Error> {
    let prof = section(db, s)?;
    let mut m = Mesh::default();
    for p in paths(db, s)? {
        merge(&mut m, &sweep::sweep(&p, &prof).map_err(geom("sweep"))?);
    }
    Ok(m)
}

fn write_sweep(db: &Connection, s: &SweepSpec, id: Option<i64>) -> Result<i64, Error> {
    if ![s.z, s.dx, s.dy].iter().all(|v| v.is_finite()) {
        return invalid("sweep: non-finite offsets");
    }
    let level = sweep_level(db, &s.host)?;
    let (hk, hosts, edges) = host_text(&s.host);
    db.execute(
        "INSERT OR REPLACE INTO sweep(id,kind,level,profile,host_kind,hosts,edges,side,z,dx,dy,material) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            id,
            s.kind.name(),
            level,
            s.profile,
            hk,
            hosts,
            edges,
            s.side as i64,
            s.z,
            s.dx,
            s.dy,
            s.material
        ],
    )?;
    let id = db.last_insert_rowid();
    let m = sweep_body(db, s)?;
    if m.triangles.is_empty() {
        return invalid("sweep: host has no edges to follow");
    }
    Ok(id)
}

// ---------------------------------------------------------------- skylights, soffit, taper

#[derive(Clone, Debug, PartialEq)]
pub struct SkylightSpec {
    pub roof: i64,
    /// Centre in plan.
    pub center: (f64, f64),
    /// Across the slope.
    pub width: f64,
    /// Along the slope (true length).
    pub length: f64,
    /// Box height above the roof.
    pub curb: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaperSpec {
    pub level: i64,
    pub slab: Option<i64>,
    pub ring: Vec<(f64, f64)>,
    /// Insulation underside above the level.
    pub offset: f64,
    /// Slope, m/m.
    pub slope: f64,
    pub t_min: f64,
    pub axis: (f64, f64),
    pub material: Option<String>,
}

fn skylight_spec(db: &Connection, id: i64) -> Result<SkylightSpec, Error> {
    db.query_row(
        "SELECT roof, x, y, width, length, curb FROM skylight WHERE id=?1",
        [id],
        |r| {
            Ok(SkylightSpec {
                roof: r.get(0)?,
                center: (r.get(1)?, r.get(2)?),
                width: r.get(3)?,
                length: r.get(4)?,
                curb: r.get(5)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("no such skylight {id}")))
}

/// Skylight frame 50 mm, glass 24 mm.
const SKY_FRAME: f64 = 0.05;
const SKY_GLASS: f64 = 0.024;

/// Bodies of a roof's fitted skylights: (id, box with frame, glass).
fn skylight_bodies(db: &Connection, roof: i64) -> Result<Vec<(i64, Mesh, Mesh)>, Error> {
    let s = roofs::load(db, roof)?;
    let t = thickness(db, s.slab_type)?;
    Ok(lit(db, roof, &s)?
        .cuts
        .into_iter()
        .map(|(k, corners, n, curb)| {
            let (f, g) = roof_shape::skylight_frame(&corners, n, t, curb, SKY_FRAME, SKY_GLASS);
            (k, f, g)
        })
        .collect())
}

fn skylight_body(db: &Connection, id: i64) -> Result<(Mesh, Mesh), Error> {
    let k = skylight_spec(db, id)?;
    skylight_bodies(db, k.roof)?
        .into_iter()
        .find(|b| b.0 == id)
        .map(|b| (b.1, b.2))
        .ok_or_else(|| Error::Invalid(format!("skylight {id} does not fit the roof")))
}

fn soffit_body(db: &Connection, id: i64) -> Result<(Mesh, f64), Error> {
    let (roof, drop_h, t): (i64, f64, f64) = db
        .query_row(
            "SELECT roof, drop_h, thickness FROM soffit WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no such soffit {id}")))?;
    let s = roofs::load(db, roof)?;
    let RoofKind::Footprint { ring, .. } = &s.kind else {
        return invalid("soffit: only footprint roofs have a support line");
    };
    let r = cut(db, roof, &s)?;
    let support: Vec<V2> = ring.iter().map(|p| v2(p.0, p.1)).collect();
    // soffit top is under the roof underside at the eave (t/cos θ of the slope, not t)
    let tr = thickness(db, s.slab_type)?;
    let under = r
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Eave)
        .filter_map(|l| {
            let d = v2(l.b.x - l.a.x, l.b.y - l.a.y);
            let len = d.len();
            if len < 1e-9 {
                return None;
            }
            let m = v2(l.a.x + l.b.x, l.a.y + l.b.y) * 0.5;
            let side = v2(-d.y, d.x) * (1e-4 / len);
            r.underside_at(m + side, tr)
                .or_else(|| r.underside_at(m - side, tr))
        })
        .fold(f64::INFINITY, f64::min);
    let z_top = if under.is_finite() {
        under
    } else {
        r.eave_z - tr
    } - drop_h;
    roof_shape::soffit(&r, &support, z_top, t).map_err(geom("soffit"))
}

fn taper_spec(db: &Connection, id: i64) -> Result<(TaperSpec, Vec<(i64, (f64, f64))>), Error> {
    type Row = (
        i64,
        Option<i64>,
        String,
        f64,
        f64,
        f64,
        f64,
        f64,
        Option<String>,
    );
    let r: Row = db
        .query_row(
            "SELECT level, slab, ring, offset, slope, tmin, ax, ay, material FROM roof_taper WHERE id=?1",
            [id],
            |r| {
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
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no such taper {id}")))?;
    let mut st = db.prepare_cached("SELECT id, x, y FROM roof_drain WHERE taper=?1 ORDER BY id")?;
    let drains: Vec<(i64, (f64, f64))> = st
        .query_map([id], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<Result<_, _>>()?;
    Ok((
        TaperSpec {
            level: r.0,
            slab: r.1,
            ring: serde_json::from_str(&r.2)?,
            offset: r.3,
            slope: r.4,
            t_min: r.5,
            axis: (r.6, r.7),
            material: r.8,
        },
        drains,
    ))
}

fn taper_geom(db: &Connection, id: i64) -> Result<Taper, Error> {
    let (s, d) = taper_spec(db, id)?;
    taper_of(db, &s, d.iter().map(|x| x.1))
}

fn taper_of(
    db: &Connection,
    s: &TaperSpec,
    drains: impl Iterator<Item = (f64, f64)>,
) -> Result<Taper, Error> {
    // holes of the base slab: no insulation, water goes around
    let holes = match s.slab {
        Some(sl) => crate::slabs::rings(db, sl)
            .unwrap_or_default()
            .into_iter()
            .skip(1)
            .map(|r| r.into_iter().map(|p| v2(p.0, p.1)).collect())
            .collect(),
        None => vec![],
    };
    Ok(Taper {
        ring: s.ring.iter().map(|p| v2(p.0, p.1)).collect(),
        drains: drains.map(|p| v2(p.0, p.1)).collect(),
        slope: s.slope,
        t_min: s.t_min,
        base_z: elevation(db, s.level)? + s.offset,
        axis: v2(s.axis.0, s.axis.1),
        holes,
    })
}

/// Level reveal cutters (elevations from `z_ref`), for cutouts in the view's walls.
pub(crate) fn reveal_cutters(
    db: &Connection,
    level: i64,
    z_ref: f64,
) -> Result<Vec<Cutter>, Error> {
    let mut out = vec![];
    for id in ids(
        db,
        "SELECT id FROM sweep WHERE level=?1 AND kind='reveal' ORDER BY id",
        level,
    )? {
        let Ok(s) = load_sweep(db, id) else { continue };
        let Ok(prof) = section(db, &s) else { continue };
        for p in paths(db, &s).unwrap_or_default() {
            if let Ok(c) = sweep::reveal_cutters(&p, &prof, 4, 0.05, z_ref) {
                out.extend(c);
            }
        }
    }
    Ok(out)
}

pub type Body = (&'static str, i64, Mesh, Option<Vec<(V3, V3)>>);

impl Model {
    // ------------------------------------------------------------ roof shapes

    /// An outline-roof preset (edge slopes and mansard along the outline): one operation.
    pub fn apply_roof_preset(
        &mut self,
        roof: i64,
        preset: Preset,
        slope: f64,
        upper: Option<f64>,
        break_h: f64,
    ) -> Result<(), Error> {
        let mut s = roofs::load(&self.db, roof)?;
        let RoofKind::Footprint { ring, .. } = &s.kind else {
            return invalid("roof preset: only footprint roofs");
        };
        let pts: Vec<V2> = ring.iter().map(|p| v2(p.0, p.1)).collect();
        let (slopes, mansard) = roof_shape::preset(&pts, preset, slope, upper, break_h);
        s.kind = RoofKind::Footprint {
            ring: ring.clone(),
            slopes,
            mansard,
        };
        self.update_roof(roof, &s)
    }

    /// Vault: an arc with rise `rise` over span a->b, extruded left by `depth`; one operation.
    #[allow(clippy::too_many_arguments)]
    pub fn add_barrel_roof(
        &mut self,
        level: i64,
        slab_type: i64,
        offset: f64,
        a: (f64, f64),
        b: (f64, f64),
        rise: f64,
        depth: f64,
    ) -> Result<i64, Error> {
        let span = (b.0 - a.0).hypot(b.1 - a.1);
        // steeper than 0.4 of the span (|bulge| > 0.8) the springings are almost vertical: the body at t/cos θ goes downward
        if !(span > 1e-6 && rise > 0.0 && rise <= MAX_BULGE * span / 2.0 + 1e-12) {
            return invalid("barrel roof: rise must be in (0, 0.4·span]");
        }
        let s = RoofSpec {
            level,
            slab_type,
            offset,
            overhang: 0.0,
            fascia: 0.0,
            kind: RoofKind::Extrusion {
                a,
                b,
                profile: vec![(0.0, 0.0), (span, 0.0)],
                depth,
            },
        };
        roofs::check(&self.db, &s)?;
        let bulges = vec![-2.0 * rise / span];
        self.op("roof_add", |db| {
            let id = roofs::write(db, &s, None)?;
            db.execute(
                "INSERT INTO roof_ext(roof, bulges, w0) VALUES (?1, ?2, 0)",
                params![id, serde_json::to_string(&bulges)?],
            )?;
            shape(db, id, &s)?;
            let inv = Inv::default()
                .delete("roof_ext", "roof", id)
                .delete("roof_vertex", "roof", id)
                .delete("roof", "id", id);
            let inv = crate::roof_auto::sync(db, inv, None)?;
            Ok((id, id, inv))
        })
    }

    fn set_ext(&mut self, roof: i64, kind: &str, f: impl FnOnce(&mut Ext)) -> Result<(), Error> {
        self.op(kind, |db| {
            let s = roofs::load(db, roof)?;
            if !matches!(s.kind, RoofKind::Extrusion { .. }) {
                return invalid("roof: only extruded roofs");
            }
            let mut e = ext(db, roof)?.unwrap_or((vec![], 0.0, None));
            f(&mut e);
            let inv = Inv::default()
                .delete("roof_ext", "roof", roof)
                .restore(db, "roof_ext", "roof", roof)?;
            db.execute(
                "INSERT OR REPLACE INTO roof_ext(roof, bulges, w0, join_roof) VALUES (?1,?2,?3,?4)",
                params![roof, serde_json::to_string(&e.0)?, e.1, e.2],
            )?;
            shape_strict(db, roof, &s)?;
            let inv = roofs::reattach(db, roof, inv)?;
            let inv = reattach_dependents(db, inv, roof)?;
            Ok(((), roof, inv))
        })
    }

    /// Arcs of the extrusion profile stretches and the start offset (extrusion between offsets
    /// w0 … w0 + depth).
    pub fn set_roof_arcs(&mut self, roof: i64, bulges: &[f64], w0: f64) -> Result<(), Error> {
        if !w0.is_finite() || bulges.iter().any(|b| !b.is_finite() || b.abs() > MAX_BULGE) {
            return invalid("roof arcs: |bulge| ≤ 0.8, finite offset");
        }
        let b = bulges.to_vec();
        self.set_ext(roof, "roof_ext", move |e| {
            e.0 = b;
            e.1 = w0;
        })
    }

    /// Joining an extrusion to roof `target`: the far end follows its top.
    pub fn join_roof(&mut self, roof: i64, target: i64) -> Result<(), Error> {
        if roof == target || !exists(&self.db, "roof", target)? {
            return invalid(format!("roof join: bad target {target}"));
        }
        // the target's join chain must not lead back to this roof
        let mut t = target;
        let mut seen = vec![roof];
        loop {
            seen.push(t);
            match ext(&self.db, t)?.and_then(|e| e.2) {
                Some(n) if seen.contains(&n) => {
                    return invalid(format!("roof join: cycle through roof {n}"));
                }
                Some(n) => t = n,
                None => break,
            }
        }
        self.set_ext(roof, "roof_join", move |e| e.2 = Some(target))
    }

    pub fn unjoin_roof(&mut self, roof: i64) -> Result<(), Error> {
        self.set_ext(roof, "roof_join", |e| e.2 = None)
    }

    /// (arcs, offset, joined to).
    pub fn roof_ext(&self, roof: i64) -> Result<Option<(Vec<f64>, f64, Option<i64>)>, Error> {
        ext(&self.db, roof)
    }

    // ------------------------------------------------------------ profile elements

    pub fn add_sweep(&mut self, s: &SweepSpec) -> Result<i64, Error> {
        self.op("sweep_add", |db| {
            let id = write_sweep(db, s, None)?;
            Ok((id, id, Inv::default().delete("sweep", "id", id)))
        })
    }

    pub fn update_sweep(&mut self, id: i64, s: &SweepSpec) -> Result<(), Error> {
        self.op("sweep_edit", |db| {
            if !exists(db, "sweep", id)? {
                return invalid(format!("no such sweep {id}"));
            }
            let inv = Inv::default()
                .delete("sweep", "id", id)
                .restore(db, "sweep", "id", id)?;
            write_sweep(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_sweep(&mut self, id: i64) -> Result<(), Error> {
        self.op("sweep_delete", |db| {
            let inv = Inv::default().restore(db, "sweep", "id", id)?;
            if db.execute("DELETE FROM sweep WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such sweep {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn sweep(&self, id: i64) -> Result<SweepSpec, Error> {
        load_sweep(&self.db, id)
    }

    /// Profile elements of a level (None means all).
    pub fn sweeps(&self, level: Option<i64>) -> Result<Vec<i64>, Error> {
        match level {
            Some(l) => ids(
                &self.db,
                "SELECT id FROM sweep WHERE level=?1 ORDER BY id",
                l,
            ),
            None => ids(&self.db, "SELECT id FROM sweep WHERE ?1=?1 ORDER BY id", 0),
        }
    }

    pub fn sweep_paths(&self, id: i64) -> Result<Vec<Path>, Error> {
        paths(&self.db, &load_sweep(&self.db, id)?)
    }

    /// Element body (for a reveal, the removed volume).
    pub fn sweep_mesh(&self, id: i64) -> Result<Mesh, Error> {
        sweep_body(&self.db, &load_sweep(&self.db, id)?)
    }

    /// Length, section, volume (a reveal: removed from the wall), surface.
    pub fn sweep_quantities(&self, id: i64) -> Result<SweepQty, Error> {
        let s = load_sweep(&self.db, id)?;
        let prof = section(&self.db, &s)?;
        let mut q = SweepQty {
            length: 0.0,
            section: prof.area(),
            volume: 0.0,
            surface: 0.0,
        };
        for p in paths(&self.db, &s)? {
            let x = sweep::quantities(&p, &prof).map_err(geom("sweep"))?;
            q.length += x.length;
            q.surface += x.surface;
            q.volume += if s.kind.subtracts() {
                sweep::reveal_volume(&p, &prof).map_err(geom("reveal"))?
            } else {
                x.volume
            };
        }
        Ok(q)
    }

    /// Level reveal cutters for wall cutouts (elevations from `z_ref`).
    pub fn reveal_cutters(&self, level: i64, z_ref: f64) -> Result<Vec<Cutter>, Error> {
        reveal_cutters(&self.db, level, z_ref)
    }

    /// Profile library: built-in, overridden/extended by file rows;
    /// (name, kind, profile, built-in).
    pub fn sweep_profiles(&self) -> Result<Vec<(String, SweepKind, SectionProfile, bool)>, Error> {
        let mut out: Vec<(String, SweepKind, SectionProfile, bool)> = sweep::library()
            .into_iter()
            .map(|l| (l.0.to_string(), l.1, l.2, true))
            .collect();
        let mut st = self
            .db
            .prepare("SELECT name, kind, pts FROM sweep_profile ORDER BY name")?;
        let rows: Vec<(String, String, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (name, k, pts) in rows {
            let (Some(kind), Some(p)) = (SweepKind::parse(&k), SectionProfile::from_json(&pts))
            else {
                continue;
            };
            match out.iter_mut().find(|o| o.0 == name) {
                Some(o) => *o = (name, kind, p, true),
                None => out.push((name, kind, p, false)),
            }
        }
        Ok(out)
    }

    /// Create/replace a profile (JSON `[[x, y, bulge], ...]`).
    pub fn set_sweep_profile(
        &mut self,
        name: &str,
        kind: SweepKind,
        p: &SectionProfile,
    ) -> Result<(), Error> {
        if name.trim().is_empty() {
            return invalid("profile: empty name");
        }
        p.validate().map_err(geom("profile"))?;
        let pts = p.to_json();
        self.op("sweep_profile", |db| {
            let old: Option<i64> = db
                .query_row("SELECT id FROM sweep_profile WHERE name=?1", [name], |r| {
                    r.get(0)
                })
                .optional()?;
            let inv = match old {
                Some(id) => Inv::default().delete("sweep_profile", "id", id).restore(
                    db,
                    "sweep_profile",
                    "id",
                    id,
                )?,
                None => Inv::default(),
            };
            db.execute(
                "INSERT INTO sweep_profile(name, kind, pts) VALUES (?1,?2,?3) \
                 ON CONFLICT(name) DO UPDATE SET kind=excluded.kind, pts=excluded.pts",
                params![name, kind.name(), pts],
            )?;
            let id: i64 =
                db.query_row("SELECT id FROM sweep_profile WHERE name=?1", [name], |r| {
                    r.get(0)
                })?;
            let inv = if old.is_none() {
                inv.delete("sweep_profile", "id", id)
            } else {
                inv
            };
            Ok(((), id, inv))
        })
    }

    /// Delete a file profile; one in use only if a built-in with the same name exists.
    pub fn delete_sweep_profile(&mut self, name: &str) -> Result<(), Error> {
        let builtin = sweep::library().iter().any(|l| l.0 == name);
        self.op("sweep_profile", |db| {
            let id: i64 = db
                .query_row("SELECT id FROM sweep_profile WHERE name=?1", [name], |r| {
                    r.get(0)
                })
                .optional()?
                .ok_or_else(|| Error::Invalid(format!("no such profile {name}")))?;
            let used: i64 =
                db.query_row("SELECT count(*) FROM sweep WHERE profile=?1", [name], |r| {
                    r.get(0)
                })?;
            if used > 0 && !builtin {
                return invalid(format!("profile {name} is used by {used} sweep(s)"));
            }
            let inv = Inv::default().restore(db, "sweep_profile", "id", id)?;
            db.execute("DELETE FROM sweep_profile WHERE id=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    // ------------------------------------------------------------ skylights and soffit

    pub fn add_skylight(&mut self, k: &SkylightSpec) -> Result<i64, Error> {
        if !(k.curb >= 0.0 && k.curb.is_finite()) {
            return invalid("skylight: curb");
        }
        self.op("roof_skylight", |db| {
            let s = roofs::load(db, k.roof)?;
            roof_shape::cut_opening(
                &cut(db, k.roof, &s)?,
                &Opening {
                    center: v2(k.center.0, k.center.1),
                    width: k.width,
                    length: k.length,
                },
            )
            .map_err(geom("skylight"))?;
            db.execute(
                "INSERT INTO skylight(roof, x, y, width, length, curb) VALUES (?1,?2,?3,?4,?5,?6)",
                params![k.roof, k.center.0, k.center.1, k.width, k.length, k.curb],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("skylight", "id", id)))
        })
    }

    pub fn delete_skylight(&mut self, id: i64) -> Result<(), Error> {
        self.op("roof_skylight", |db| {
            let inv = Inv::default().restore(db, "skylight", "id", id)?;
            if db.execute("DELETE FROM skylight WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such skylight {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn skylight(&self, id: i64) -> Result<SkylightSpec, Error> {
        skylight_spec(&self.db, id)
    }

    pub fn skylights(&self, roof: i64) -> Result<Vec<i64>, Error> {
        Ok(openings(&self.db, roof)?.into_iter().map(|o| o.0).collect())
    }

    /// Skylights and soffits of a level's roofs.
    pub fn level_skylights(&self, level: i64) -> Result<Vec<i64>, Error> {
        ids(
            &self.db,
            "SELECT k.id FROM skylight k JOIN roof r ON r.id=k.roof WHERE r.level=?1 ORDER BY k.id",
            level,
        )
    }

    pub fn level_soffits(&self, level: i64) -> Result<Vec<i64>, Error> {
        ids(
            &self.db,
            "SELECT s.id FROM soffit s JOIN roof r ON r.id=s.roof WHERE r.level=?1 ORDER BY s.id",
            level,
        )
    }

    /// (box with frame, glass).
    pub fn skylight_mesh(&self, id: i64) -> Result<(Mesh, Mesh), Error> {
        skylight_body(&self.db, id)
    }

    /// Soffit of an outline roof's overhang: from the eave to the bearing line.
    pub fn add_soffit(
        &mut self,
        roof: i64,
        drop_h: f64,
        thickness: f64,
        material: Option<&str>,
    ) -> Result<i64, Error> {
        if !(thickness > 0.0 && drop_h >= 0.0 && drop_h.is_finite() && thickness.is_finite()) {
            return invalid("soffit: thickness > 0, drop ≥ 0");
        }
        self.op("soffit_add", |db| {
            db.execute(
                "INSERT INTO soffit(roof, drop_h, thickness, material) VALUES (?1,?2,?3,?4)",
                params![roof, drop_h, thickness, material],
            )?;
            let id = db.last_insert_rowid();
            soffit_body(db, id)?;
            Ok((id, id, Inv::default().delete("soffit", "id", id)))
        })
    }

    pub fn delete_soffit(&mut self, id: i64) -> Result<(), Error> {
        self.op("soffit_delete", |db| {
            let inv = Inv::default().restore(db, "soffit", "id", id)?;
            if db.execute("DELETE FROM soffit WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such soffit {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn soffits(&self, roof: i64) -> Result<Vec<i64>, Error> {
        ids(
            &self.db,
            "SELECT id FROM soffit WHERE roof=?1 ORDER BY id",
            roof,
        )
    }

    /// (body, area).
    pub fn soffit_mesh(&self, id: i64) -> Result<(Mesh, f64), Error> {
        soffit_body(&self.db, id)
    }

    // ------------------------------------------------------------ tapered insulation

    pub fn add_taper(&mut self, s: &TaperSpec) -> Result<i64, Error> {
        taper_of(&self.db, s, std::iter::empty())?
            .facets()
            .map_err(geom("taper"))?;
        let ring = serde_json::to_string(&s.ring)?;
        self.op("taper_add", |db| {
            db.execute(
                "INSERT INTO roof_taper(level, slab, ring, offset, slope, tmin, ax, ay, material) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    s.level, s.slab, ring, s.offset, s.slope, s.t_min, s.axis.0, s.axis.1,
                    s.material
                ],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("roof_taper", "id", id)))
        })
    }

    /// A taper over the slab's outer outline, bottom at its top, axis along the long side.
    pub fn add_taper_on_slab(&mut self, slab: i64, slope: f64, t_min: f64) -> Result<i64, Error> {
        let (level, offset): (i64, f64) = self
            .db
            .query_row("SELECT level, offset FROM slab WHERE id=?1", [slab], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no such slab {slab}")))?;
        let ring = crate::slabs::rings(&self.db, slab)?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Invalid(format!("slab {slab} has no outline")))?;
        let n = ring.len();
        let axis = (0..n)
            .map(|i| {
                let (p, q) = (ring[i], ring[(i + 1) % n]);
                (q.0 - p.0, q.1 - p.1)
            })
            .max_by(|a, b| a.0.hypot(a.1).total_cmp(&b.0.hypot(b.1)))
            .unwrap_or((1.0, 0.0));
        self.add_taper(&TaperSpec {
            level,
            slab: Some(slab),
            ring,
            offset,
            slope,
            t_min,
            axis,
            material: None,
        })
    }

    pub fn delete_taper(&mut self, id: i64) -> Result<(), Error> {
        self.op("taper_delete", |db| {
            let inv = Inv::default()
                .restore(db, "roof_taper", "id", id)?
                .restore(db, "roof_drain", "taper", id)?;
            if db.execute("DELETE FROM roof_taper WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such taper {id}"));
            }
            db.execute("DELETE FROM roof_drain WHERE taper=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    pub fn add_drain(&mut self, taper: i64, p: (f64, f64)) -> Result<i64, Error> {
        self.op("taper_drain", |db| {
            let (s, d) = taper_spec(db, taper)?;
            taper_of(db, &s, d.iter().map(|x| x.1).chain([p]))?
                .facets()
                .map_err(geom("drain"))?;
            db.execute(
                "INSERT INTO roof_drain(taper, x, y) VALUES (?1,?2,?3)",
                params![taper, p.0, p.1],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, taper, Inv::default().delete("roof_drain", "id", id)))
        })
    }

    pub fn delete_drain(&mut self, id: i64) -> Result<(), Error> {
        self.op("taper_drain", |db| {
            let inv = Inv::default().restore(db, "roof_drain", "id", id)?;
            if db.execute("DELETE FROM roof_drain WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such drain {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn taper(&self, id: i64) -> Result<(TaperSpec, Vec<(i64, (f64, f64))>), Error> {
        taper_spec(&self.db, id)
    }

    pub fn tapers(&self, level: i64) -> Result<Vec<i64>, Error> {
        ids(
            &self.db,
            "SELECT id FROM roof_taper WHERE level=?1 ORDER BY id",
            level,
        )
    }

    pub fn taper_quantities(&self, id: i64) -> Result<TaperQty, Error> {
        taper_geom(&self.db, id)?
            .quantities()
            .map_err(geom("taper"))
    }

    pub fn taper_mesh(&self, id: i64) -> Result<Mesh, Error> {
        taper_geom(&self.db, id)?.mesh().map_err(geom("taper"))
    }

    // ------------------------------------------------------------ editing and deletion

    /// New position/size of a skylight; one that does not fit the roof is an error.
    pub fn update_skylight(&mut self, id: i64, k: &SkylightSpec) -> Result<(), Error> {
        if !(k.curb >= 0.0 && k.curb.is_finite()) {
            return invalid("skylight: curb");
        }
        self.op("roof_skylight", |db| {
            let inv = Inv::default()
                .delete("skylight", "id", id)
                .restore(db, "skylight", "id", id)?;
            let n = db.execute(
                "UPDATE skylight SET roof=?2, x=?3, y=?4, width=?5, length=?6, curb=?7 WHERE id=?1",
                params![id, k.roof, k.center.0, k.center.1, k.width, k.length, k.curb],
            )?;
            if n == 0 {
                return invalid(format!("no such skylight {id}"));
            }
            let s = roofs::load(db, k.roof)?;
            if let Some((_, why)) = lit(db, k.roof, &s)?.skipped.into_iter().find(|x| x.0 == id) {
                return invalid(format!("skylight {id}: {why}"));
            }
            Ok(((), id, inv))
        })
    }

    /// (roof, downward offset, thickness, material).
    pub fn soffit(&self, id: i64) -> Result<(i64, f64, f64, Option<String>), Error> {
        self.db
            .query_row(
                "SELECT roof, drop_h, thickness, material FROM soffit WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no such soffit {id}")))
    }

    pub fn update_soffit(
        &mut self,
        id: i64,
        drop_h: f64,
        thickness: f64,
        material: Option<&str>,
    ) -> Result<(), Error> {
        if !(thickness > 0.0 && drop_h >= 0.0 && drop_h.is_finite() && thickness.is_finite()) {
            return invalid("soffit: thickness > 0, drop ≥ 0");
        }
        self.op("soffit_edit", |db| {
            let inv = Inv::default()
                .delete("soffit", "id", id)
                .restore(db, "soffit", "id", id)?;
            let n = db.execute(
                "UPDATE soffit SET drop_h=?2, thickness=?3, material=?4 WHERE id=?1",
                params![id, drop_h, thickness, material],
            )?;
            if n == 0 {
                return invalid(format!("no such soffit {id}"));
            }
            soffit_body(db, id)?;
            Ok(((), id, inv))
        })
    }

    pub fn update_taper(&mut self, id: i64, s: &TaperSpec) -> Result<(), Error> {
        let ring = serde_json::to_string(&s.ring)?;
        self.op("taper_edit", |db| {
            let inv = Inv::default().delete("roof_taper", "id", id).restore(
                db,
                "roof_taper",
                "id",
                id,
            )?;
            let n = db.execute(
                "UPDATE roof_taper SET level=?2, slab=?3, ring=?4, offset=?5, slope=?6, tmin=?7, \
                 ax=?8, ay=?9, material=?10 WHERE id=?1",
                params![
                    id, s.level, s.slab, ring, s.offset, s.slope, s.t_min, s.axis.0, s.axis.1,
                    s.material
                ],
            )?;
            if n == 0 {
                return invalid(format!("no such taper {id}"));
            }
            taper_geom(db, id)?.facets().map_err(geom("taper"))?;
            Ok(((), id, inv))
        })
    }

    pub fn move_drain(&mut self, id: i64, p: (f64, f64)) -> Result<(), Error> {
        self.op("taper_drain", |db| {
            let inv = Inv::default().delete("roof_drain", "id", id).restore(
                db,
                "roof_drain",
                "id",
                id,
            )?;
            let n = db.execute(
                "UPDATE roof_drain SET x=?2, y=?3 WHERE id=?1",
                params![id, p.0, p.1],
            )?;
            if n == 0 {
                return invalid(format!("no such drain {id}"));
            }
            let taper: i64 =
                db.query_row("SELECT taper FROM roof_drain WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
            taper_geom(db, taper)?.facets().map_err(geom("drain"))?;
            Ok(((), taper, inv))
        })
    }

    /// Deleting an edge element by body kind (`sweep`, `skylight`, `skylight_glass`, `soffit`,
    /// `taper`, `drain`), for selection in 3D.
    pub fn delete_roof_edge(&mut self, kind: &str, id: i64) -> Result<(), Error> {
        match kind {
            "sweep" => self.delete_sweep(id),
            "skylight" | "skylight_glass" => self.delete_skylight(id),
            "soffit" => self.delete_soffit(id),
            "taper" => self.delete_taper(id),
            "drain" => self.delete_drain(id),
            _ => invalid(format!("not a roof edge element: {kind}")),
        }
    }

    // ------------------------------------------------------------ views

    /// Bodies for plans, sections and 3D: profiles (except reveals), skylights (`skylight` is the box with
    /// frame, `skylight_glass` the glass), soffits, tapers; elevations are absolute. Broken ones
    /// are skipped (see [`Model::roof_edge_issues`]). Cached until the next file change.
    pub fn roof_edge_bodies(&self, level: Option<i64>) -> Result<Vec<Body>, Error> {
        let rev = self.db.total_changes();
        let mut memo = self.memo.0.lock().unwrap_or_else(|e| e.into_inner());
        if memo.as_ref().is_none_or(|m| m.0 != rev) {
            *memo = Some((rev, all_bodies(&self.db)?));
        }
        let all = memo.as_ref().map(|m| &m.1[..]).unwrap_or(&[]);
        Ok(all
            .iter()
            .filter(|b| level.is_none_or(|l| l == b.0))
            .map(|b| b.1.clone())
            .collect())
    }

    /// Edge element issues: dangling profile hosts, skylights that no longer fit, dropped
    /// roof joins, bodies that cannot be built.
    pub fn roof_edge_issues(&self) -> Result<Vec<Issue>, Error> {
        issues(&self.db)
    }
}

/// Edge body cache: (file change number, [(level, body)]).
#[derive(Default)]
pub(crate) struct Memo(std::sync::Mutex<Option<(u64, Vec<(i64, Body)>)>>);

fn level_of(db: &Connection, table: &str, id: i64) -> Result<Option<i64>, Error> {
    Ok(db
        .query_row(
            &format!("SELECT level FROM {table} WHERE id=?1"),
            [id],
            |r| r.get(0),
        )
        .optional()?)
}

fn all_bodies(db: &Connection) -> Result<Vec<(i64, Body)>, Error> {
    let mut out: Vec<(i64, Body)> = vec![];
    for id in ids(db, "SELECT id FROM sweep WHERE ?1=?1 ORDER BY id", 0)? {
        let Ok(s) = load_sweep(db, id) else { continue };
        if s.kind.subtracts() {
            continue;
        }
        let Some(l) = level_of(db, "sweep", id)? else {
            continue;
        };
        if let Ok(m) = sweep_body(db, &s) {
            if !m.triangles.is_empty() {
                out.push((l, ("sweep", id, m, None)));
            }
        }
    }
    for (roof, l) in pairs(
        db,
        "SELECT DISTINCT r.id, r.level FROM roof r JOIN skylight k ON k.roof=r.id ORDER BY r.id",
    )? {
        for (id, f, g) in skylight_bodies(db, roof).unwrap_or_default() {
            out.push((l, ("skylight", id, f, None)));
            out.push((l, ("skylight_glass", id, g, None)));
        }
    }
    for (id, l) in pairs(
        db,
        "SELECT s.id, r.level FROM soffit s JOIN roof r ON r.id=s.roof ORDER BY s.id",
    )? {
        if let Ok((m, _)) = soffit_body(db, id) {
            out.push((l, ("soffit", id, m, None)));
        }
    }
    for (id, l) in pairs(db, "SELECT id, level FROM roof_taper ORDER BY id")? {
        if let Ok(m) = taper_geom(db, id).and_then(|t| t.mesh().map_err(geom("taper"))) {
            out.push((l, ("taper", id, m, None)));
        }
    }
    Ok(out)
}

fn pairs(db: &Connection, sql: &str) -> Result<Vec<(i64, i64)>, Error> {
    let mut st = db.prepare(sql)?;
    let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(r.collect::<Result<_, _>>()?)
}

/// Whether a roof has a fascia profile (then the roof's `fascia` parameter does not go into the area).
pub(crate) fn has_fascia_sweep(db: &Connection, roof: i64) -> Result<bool, Error> {
    Ok(hosted(db, "roof", roof)?.into_iter().any(|id| {
        db.query_row("SELECT kind FROM sweep WHERE id=?1", [id], |r| {
            r.get::<_, String>(0)
        })
        .is_ok_and(|k| k == SweepKind::Fascia.name())
    }))
}

pub(crate) fn issues(db: &Connection) -> Result<Vec<Issue>, Error> {
    let mut out = vec![];
    let mut push = |table: &str, id: i64, problem: String| {
        out.push(Issue {
            table: table.into(),
            id,
            problem,
        })
    };
    let rows: Vec<(i64, String, String)> = {
        let mut st = db.prepare("SELECT id, host_kind, hosts FROM sweep ORDER BY id")?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        r.collect::<Result<_, _>>()?
    };
    for (id, kind, hosts) in rows {
        let Ok(hs) = serde_json::from_str::<Vec<i64>>(&hosts) else {
            push("sweep", id, format!("hosts: bad JSON {hosts}"));
            continue;
        };
        if hs.is_empty() {
            push("sweep", id, "no hosts".into());
        }
        for h in hs {
            if !exists(db, &kind, h)? {
                push("sweep", id, format!("hosts -> missing {kind} {h}"));
            }
        }
        if let Err(e) = load_sweep(db, id).and_then(|s| sweep_body(db, &s)) {
            push("sweep", id, format!("cannot be built: {e}"));
        }
    }
    for roof in ids(
        db,
        "SELECT DISTINCT roof FROM skylight WHERE ?1=?1 ORDER BY roof",
        0,
    )? {
        let Ok(s) = roofs::load(db, roof) else {
            continue;
        };
        for (k, why) in lit(db, roof, &s)?.skipped {
            push("skylight", k, format!("does not fit roof {roof}: {why}"));
        }
    }
    for roof in ids(
        db,
        "SELECT roof FROM roof_ext WHERE join_roof IS NOT NULL AND ?1=?1 ORDER BY roof",
        0,
    )? {
        let Ok(s) = roofs::load(db, roof) else {
            continue;
        };
        if let (_, Some(w)) = own(db, roof, &s, true)? {
            push("roof_ext", roof, format!("{w}; unjoined shape used"));
        }
    }
    for id in ids(db, "SELECT id FROM soffit WHERE ?1=?1 ORDER BY id", 0)? {
        if let Err(e) = soffit_body(db, id) {
            push("soffit", id, format!("cannot be built: {e}"));
        }
    }
    for id in ids(db, "SELECT id FROM roof_taper WHERE ?1=?1 ORDER BY id", 0)? {
        if let Err(e) = taper_geom(db, id).and_then(|t| t.facets().map_err(geom("taper"))) {
            push("roof_taper", id, format!("cannot be built: {e}"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, SlabSpec, WallSpec};

    fn near(a: f64, b: f64, tol: f64, what: &str) {
        assert!(
            (a - b).abs() <= tol * b.abs().max(1.0),
            "{what}: {a} vs {b}"
        );
    }

    fn setup() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L0", 0.0).unwrap();
        let st = m
            .add_slab_type(
                "Roof",
                &[LayerSpec {
                    material: "Deck".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        (m, l, st)
    }

    fn rect(l: f64, w: f64) -> Vec<(f64, f64)> {
        vec![(0.0, 0.0), (l, 0.0), (l, w), (0.0, w)]
    }

    fn footprint(
        level: i64,
        st: i64,
        ring: Vec<(f64, f64)>,
        slopes: Vec<Option<f64>>,
        overhang: f64,
    ) -> RoofSpec {
        RoofSpec {
            level,
            slab_type: st,
            offset: 3.0,
            overhang,
            fascia: 0.0,
            kind: RoofKind::Footprint {
                ring,
                slopes,
                mansard: None,
            },
        }
    }

    fn count(m: &Model, t: &str) -> i64 {
        m.db.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn tables_exist_at_current_version() {
        let (m, _, _) = setup();
        let v: i64 =
            m.db.query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, crate::schema::VERSION);
        for t in [
            "roof_ext",
            "sweep_profile",
            "sweep",
            "skylight",
            "soffit",
            "roof_taper",
            "roof_drain",
        ] {
            assert_eq!(count(&m, t), 0, "{t}");
        }
        ensure(&m.db).unwrap();
    }

    #[test]
    fn presets_go_through_one_undo_step() {
        let (mut m, l, st) = setup();
        let id = m
            .add_roof(&footprint(l, st, rect(10.0, 6.0), vec![Some(30.0); 4], 0.0))
            .unwrap();
        m.apply_roof_preset(id, Preset::Gable, 30.0, None, 0.0)
            .unwrap();
        near(
            m.roof_quantities(id).unwrap().geom.ridge,
            10.0,
            1e-9,
            "gable ridge",
        );
        m.apply_roof_preset(id, Preset::Shed, 20.0, None, 0.0)
            .unwrap();
        let q = m.roof_quantities(id).unwrap().geom;
        near(q.area, 60.0 / 20f64.to_radians().cos(), 1e-9, "shed area");
        m.undo().unwrap();
        near(
            m.roof_quantities(id).unwrap().geom.ridge,
            10.0,
            1e-9,
            "undo to gable",
        );
        m.apply_roof_preset(id, Preset::Mansard, 70.0, Some(20.0), 1.5)
            .unwrap();
        assert!(matches!(
            m.roof(id).unwrap().kind,
            RoofKind::Footprint {
                mansard: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn barrel_roof_area_volume_and_undo() {
        let (mut m, l, st) = setup();
        let id = m
            .add_barrel_roof(l, st, 4.0, (0.0, 0.0), (6.0, 0.0), 2.4, 10.0)
            .unwrap();
        let q = m.roof_quantities(id).unwrap();
        // shallow vault |bulge| = 0.8: arc R·θ, segment R²/2·(θ − sin θ)
        let th = 4.0 * 0.8f64.atan();
        let r = 3.0 / (th / 2.0).sin();
        near(q.geom.area, r * th * 10.0, 1e-3, "vault area");
        near(
            q.geom.volume_under,
            r * r / 2.0 * (th - th.sin()) * 10.0,
            2e-3,
            "vault volume",
        );
        near(q.layers[0].2, q.geom.area, 1e-12, "layer area");
        // start offset: the plan shifts, the area is the same
        m.set_roof_arcs(id, &[-0.8], 2.0).unwrap();
        let g = m.roof_geometry(id).unwrap();
        assert!(g.top_at(v2(3.0, 1.0)).is_none());
        assert!(g.top_at(v2(3.0, 11.0)).is_some());
        m.undo().unwrap();
        assert_eq!(m.roof_ext(id).unwrap().unwrap().1, 0.0);
        m.undo().unwrap();
        assert_eq!(count(&m, "roof"), 0);
        assert_eq!(count(&m, "roof_ext"), 0);
        assert!(m
            .add_barrel_roof(l, st, 4.0, (0.0, 0.0), (6.0, 0.0), 4.0, 10.0)
            .is_err());
    }

    #[test]
    fn join_extends_gable_to_main_roof_and_unjoin_restores() {
        let (mut m, l, st) = setup();
        // main: slopes z = 3 + 0.5·(distance from the eave), ridge along x at y = 5
        let deg = 0.5f64.atan().to_degrees();
        let main = m
            .add_roof(&footprint(
                l,
                st,
                rect(20.0, 10.0),
                vec![Some(deg), None, Some(deg), None],
                0.0,
            ))
            .unwrap();
        let wing = m
            .add_roof(&RoofSpec {
                level: l,
                slab_type: st,
                offset: 3.0,
                overhang: 0.0,
                fascia: 0.0,
                kind: RoofKind::Extrusion {
                    a: (8.0, -4.0),
                    b: (12.0, -4.0),
                    profile: vec![(0.0, 0.0), (2.0, 2.0), (4.0, 0.0)],
                    depth: 1.0,
                },
            })
            .unwrap();
        near(
            m.roof_quantities(wing).unwrap().geom.plan_area,
            4.0,
            1e-9,
            "before",
        );
        m.join_roof(wing, main).unwrap();
        // slope: far edge from y = 0 (z = 0) to y = 4 (ridge z = 2) -> plan 2·(8 + 4)
        let q = m.roof_quantities(wing).unwrap().geom;
        near(q.plan_area, 24.0, 1e-9, "joined plan");
        near(q.area, 24.0 * 2f64.sqrt(), 1e-9, "joined area");
        m.unjoin_roof(wing).unwrap();
        near(
            m.roof_quantities(wing).unwrap().geom.plan_area,
            4.0,
            1e-9,
            "unjoined",
        );
        m.undo().unwrap();
        near(
            m.roof_quantities(wing).unwrap().geom.plan_area,
            24.0,
            1e-9,
            "undo unjoin",
        );
        // deleting the target removes the join; undo restores it
        m.delete_roof(main).unwrap();
        assert_eq!(m.roof_ext(wing).unwrap().unwrap().2, None);
        m.undo().unwrap();
        assert_eq!(m.roof_ext(wing).unwrap().unwrap().2, Some(main));
        assert!(m.join_roof(wing, wing).is_err());
        m.assert_integrity();
    }

    #[test]
    fn skylight_cuts_roof_and_cascades_with_roof() {
        let (mut m, l, st) = setup();
        let id = m
            .add_roof(&footprint(l, st, rect(10.0, 6.0), vec![Some(30.0); 4], 0.5))
            .unwrap();
        let a0 = m.roof_quantities(id).unwrap().geom.area;
        let k = m
            .add_skylight(&SkylightSpec {
                roof: id,
                center: (5.0, 1.0),
                width: 1.0,
                length: 1.2,
                curb: 0.15,
            })
            .unwrap();
        let q = m.roof_quantities(id).unwrap();
        near(q.geom.area, a0 - 1.2, 1e-9, "cut area");
        near(q.layers[0].3, (a0 - 1.2) * 0.2, 1e-9, "layer volume");
        let (frame, glass) = m.skylight_mesh(k).unwrap();
        assert!(frame.volume() > 0.0 && glass.volume() > 0.0);
        assert!(m
            .add_skylight(&SkylightSpec {
                center: (50.0, 1.0),
                ..m.skylight(k).unwrap()
            })
            .is_err());
        m.delete_roof(id).unwrap();
        assert_eq!(count(&m, "skylight"), 0);
        m.undo().unwrap();
        assert_eq!(m.skylights(id).unwrap(), vec![k]);
        m.undo().unwrap();
        near(
            m.roof_quantities(id).unwrap().geom.area,
            a0,
            1e-9,
            "undo skylight",
        );
    }

    #[test]
    fn soffit_area_under_overhang() {
        let (mut m, l, st) = setup();
        let id = m
            .add_roof(&footprint(l, st, rect(10.0, 6.0), vec![Some(30.0); 4], 0.5))
            .unwrap();
        let s = m.add_soffit(id, 0.0, 0.02, Some("Board")).unwrap();
        let (mesh, a) = m.soffit_mesh(s).unwrap();
        near(a, 11.0 * 7.0 - 60.0, 1e-9, "soffit area");
        near(mesh.volume(), a * 0.02, 1e-9, "soffit volume");
        let flat = m
            .add_roof(&footprint(l, st, rect(10.0, 6.0), vec![Some(30.0); 4], 0.0))
            .unwrap();
        assert!(m.add_soffit(flat, 0.0, 0.02, None).is_err());
        assert_eq!(m.soffits(flat).unwrap(), Vec::<i64>::new());
    }

    #[test]
    fn fascia_and_slab_edge_follow_closed_outline_with_miters() {
        let (mut m, l, st) = setup();
        let roof = m
            .add_roof(&footprint(l, st, rect(10.0, 6.0), vec![Some(30.0); 4], 0.0))
            .unwrap();
        let f = m
            .add_sweep(&SweepSpec {
                kind: SweepKind::Fascia,
                profile: "fascia_200".into(),
                host: SweepHost::Roof { roof, rakes: true },
                side: -1,
                z: 0.0,
                dx: 0.0,
                dy: 0.0,
                material: None,
            })
            .unwrap();
        let q = m.sweep_quantities(f).unwrap();
        near(q.length, 32.0, 1e-9, "fascia length");
        // closed rectangle, section h × w outward: V = h·(P·w + 4w²)
        let (h, w) = (0.2, 0.025);
        near(
            q.volume,
            h * (32.0 * w + 4.0 * w * w),
            1e-9,
            "fascia volume",
        );
        assert!(m.sweep_mesh(f).unwrap().is_watertight());
        let slab = m
            .add_slab(&SlabSpec {
                level: l,
                slab_type: st,
                offset: 0.0,
                outer: rect(8.0, 5.0),
                holes: vec![],
            })
            .unwrap();
        let e = m
            .add_sweep(&SweepSpec {
                kind: SweepKind::SlabEdge,
                profile: "slab_edge_200x450".into(),
                host: SweepHost::Slab(slab),
                side: -1,
                z: 0.0,
                dx: 0.0,
                dy: 0.0,
                material: None,
            })
            .unwrap();
        let q = m.sweep_quantities(e).unwrap();
        near(
            q.volume,
            0.45 * (26.0 * 0.2 + 4.0 * 0.04),
            1e-9,
            "slab edge volume",
        );
        // deleting a roof takes its fascia with it
        m.delete_roof(roof).unwrap();
        assert!(m.sweep(f).is_err());
        m.undo().unwrap();
        assert_eq!(m.sweep(f).unwrap().profile, "fascia_200");
        m.delete_sweep(e).unwrap();
        m.undo().unwrap();
        assert!(m.sweep(e).is_ok());
    }

    fn l_walls(m: &mut Model, l: i64) -> (i64, i64) {
        let wt = m
            .add_wall_type(
                "W200",
                &[LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = |a, b| WallSpec {
            a,
            b,
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        };
        let w1 = m.add_wall(&w((0.0, 0.0), (6.0, 0.0))).unwrap();
        let w2 = m.add_wall(&w((6.0, 0.0), (6.0, 4.0))).unwrap();
        (w1, w2)
    }

    #[test]
    fn wall_reveal_subtracts_and_cornice_miters_outside_corner() {
        let (mut m, l, _) = setup();
        let (w1, w2) = l_walls(&mut m, l);
        // right face along (0,0)->(6,0)->(6,4) is the outer side of the corner
        let r = m
            .add_sweep(&SweepSpec {
                kind: SweepKind::Reveal,
                profile: "reveal_20x20".into(),
                host: SweepHost::Walls(vec![w1, w2]),
                side: -1,
                z: 1.0,
                dx: 0.0,
                dy: 0.0,
                material: None,
            })
            .unwrap();
        let p = &m.sweep_paths(r).unwrap()[0];
        assert_eq!(p.pts.len(), 3);
        near(p.pts[1].x, 6.1, 1e-12, "corner x");
        near(p.pts[1].y, -0.1, 1e-12, "corner y");
        near(p.pts[0].z, 1.0, 1e-12, "reveal z");
        // a reveal removes a 0.02 × 0.02 section along the inner side of the path: lengths 6.1 + 4.1,
        // minus the square w² at the corner (the inner mitre corner of the section)
        let q = m.sweep_quantities(r).unwrap();
        let (a, wd) = (0.02 * 0.02, 0.02);
        near(
            q.volume,
            a * (6.1 + 4.1) - 0.02 * wd * wd,
            1e-9,
            "reveal volume",
        );
        let cut = m.reveal_cutters(l, 0.0).unwrap();
        assert_eq!(cut.len(), 2);
        assert!(cut
            .iter()
            .all(|c| (c.z0 - 0.99).abs() < 1e-9 && (c.z1 - 1.01).abs() < 1e-9));
        // moulding on the left face (inner corner): the path is reversed, the section faces inward
        let c = m
            .add_sweep(&SweepSpec {
                kind: SweepKind::WallSweep,
                profile: "base_100x20".into(),
                host: SweepHost::Walls(vec![w1, w2]),
                side: 1,
                z: 0.0,
                dx: 0.0,
                dy: 0.0,
                material: None,
            })
            .unwrap();
        let p = &m.sweep_paths(c).unwrap()[0];
        near(p.pts[1].x, 5.9, 1e-12, "inner corner x");
        near(p.pts[1].y, 0.1, 1e-12, "inner corner y");
        let q = m.sweep_quantities(c).unwrap();
        near(q.length, 3.9 + 5.9, 1e-9, "inner length");
        near(
            q.volume,
            0.1 * (0.02 * 9.8 - 0.02 * 0.02),
            1e-9,
            "inner miter volume",
        );
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.sweeps(Some(l)).unwrap(), Vec::<i64>::new());
    }

    #[test]
    fn custom_profile_library_round_trip() {
        let (mut m, l, _) = setup();
        let (w1, _) = l_walls(&mut m, l);
        let p = SectionProfile::rect(0.0, 0.0, 0.05, 0.3);
        m.set_sweep_profile("plinth", SweepKind::WallSweep, &p)
            .unwrap();
        let lib = m.sweep_profiles().unwrap();
        assert!(lib.iter().any(|x| x.0 == "plinth" && !x.3 && x.2 == p));
        let s = m
            .add_sweep(&SweepSpec {
                kind: SweepKind::WallSweep,
                profile: "plinth".into(),
                host: SweepHost::Walls(vec![w1]),
                side: -1,
                z: 0.0,
                dx: 0.0,
                dy: 0.0,
                material: None,
            })
            .unwrap();
        near(
            m.sweep_quantities(s).unwrap().volume,
            0.05 * 0.3 * 6.0,
            1e-9,
            "plinth",
        );
        assert!(m.delete_sweep_profile("plinth").is_err());
        // overriding a built-in profile and rollback
        m.set_sweep_profile("base_100x20", SweepKind::WallSweep, &p)
            .unwrap();
        m.undo().unwrap();
        let b = m.sweep_profiles().unwrap();
        let base = b.iter().find(|x| x.0 == "base_100x20").unwrap();
        assert_eq!(base.2, SectionProfile::rect(0.0, 0.0, 0.02, 0.1));
        assert!(m
            .set_sweep_profile("bad", SweepKind::Fascia, &SectionProfile { pts: vec![] })
            .is_err());
    }

    #[test]
    fn taper_on_slab_with_central_drain() {
        let (mut m, l, st) = setup();
        let slab = m
            .add_slab(&SlabSpec {
                level: l,
                slab_type: st,
                offset: 0.0,
                outer: vec![(-5.0, -5.0), (5.0, -5.0), (5.0, 5.0), (-5.0, 5.0)],
                holes: vec![],
            })
            .unwrap();
        let t = m.add_taper_on_slab(slab, 0.02, 0.05).unwrap();
        near(
            m.taper_quantities(t).unwrap().volume,
            100.0 * 0.05,
            1e-12,
            "flat",
        );
        let d = m.add_drain(t, (0.0, 0.0)).unwrap();
        let q = m.taper_quantities(t).unwrap();
        near(
            q.volume,
            0.05 * 100.0 + 0.02 * 8.0 * 125.0 / 3.0,
            1e-9,
            "pyramid",
        );
        near(q.t_max, 0.05 + 0.02 * 5.0, 1e-12, "t max");
        near(m.taper_mesh(t).unwrap().volume(), q.volume, 1e-9, "mesh");
        assert!(m.add_drain(t, (20.0, 0.0)).is_err());
        m.delete_drain(d).unwrap();
        m.undo().unwrap();
        assert_eq!(m.taper(t).unwrap().1.len(), 1);
        m.delete_taper(t).unwrap();
        assert_eq!(count(&m, "roof_drain"), 0);
        m.undo().unwrap();
        assert_eq!(count(&m, "roof_drain"), 1);
        let bodies = m.roof_edge_bodies(Some(l)).unwrap();
        assert!(bodies.iter().any(|b| b.0 == "taper" && b.1 == t));
    }
    fn cornice(m: &mut Model, walls: Vec<i64>, side: i8) -> Result<i64, Error> {
        m.add_sweep(&SweepSpec {
            kind: SweepKind::WallSweep,
            profile: "base_100x20".into(),
            host: SweepHost::Walls(walls),
            side,
            z: 0.0,
            dx: 0.0,
            dy: 0.0,
            material: None,
        })
    }

    #[test]
    fn open_path_gutter_merges_without_face_id_overflow() {
        let (mut m, l, st) = setup();
        let roof = m
            .add_roof(&footprint(
                l,
                st,
                rect(10.0, 6.0),
                vec![Some(30.0), None, Some(30.0), None],
                0.0,
            ))
            .unwrap();
        // gable without gables: two open eave paths, ends with separate faces
        let g = m
            .add_sweep(&SweepSpec {
                kind: SweepKind::Gutter,
                profile: "gutter_half_round_150".into(),
                host: SweepHost::Roof { roof, rakes: false },
                side: -1,
                z: 0.0,
                dx: 0.0,
                dy: 0.0,
                material: None,
            })
            .unwrap();
        assert_eq!(m.sweep_paths(g).unwrap().len(), 2);
        let mesh = m.sweep_mesh(g).unwrap();
        assert!(mesh.face_of.iter().all(|&f| f < u32::MAX / 2));
        near(
            m.sweep_quantities(g).unwrap().length,
            20.0,
            1e-9,
            "two eaves",
        );
    }

    #[test]
    fn sweep_loses_deleted_wall_and_new_wall_does_not_inherit_it() {
        let (mut m, l, _) = setup();
        let (w1, w2) = l_walls(&mut m, l);
        let c = cornice(&mut m, vec![w1, w2], -1).unwrap();
        m.delete_wall(w2).unwrap();
        assert_eq!(m.sweep(c).unwrap().host, SweepHost::Walls(vec![w1]));
        m.undo().unwrap();
        assert_eq!(m.sweep(c).unwrap().host, SweepHost::Walls(vec![w1, w2]));
        m.delete_walls(&[w1, w2]).unwrap();
        assert!(m.sweep(c).is_err());
        // the wall number is freed: a new wall with the same number does not get the cornice
        let wt: i64 =
            m.db.query_row("SELECT id FROM wall_type LIMIT 1", [], |r| r.get(0))
                .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        assert!(
            m.sweeps(Some(l)).unwrap().is_empty(),
            "wall {w} inherited a sweep"
        );
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.sweep(c).unwrap().host, SweepHost::Walls(vec![w1, w2]));
        m.assert_integrity();
    }

    #[test]
    fn taper_goes_with_its_slab() {
        let (mut m, l, st) = setup();
        let slab = m
            .add_slab(&SlabSpec {
                level: l,
                slab_type: st,
                offset: 0.0,
                outer: rect(10.0, 10.0),
                holes: vec![],
            })
            .unwrap();
        let t = m.add_taper_on_slab(slab, 0.02, 0.05).unwrap();
        m.add_drain(t, (5.0, 5.0)).unwrap();
        m.delete_slab(slab).unwrap();
        assert_eq!(count(&m, "roof_taper"), 0);
        assert_eq!(count(&m, "roof_drain"), 0);
        m.undo().unwrap();
        assert_eq!(m.taper(t).unwrap().1.len(), 1);
        m.assert_integrity();
    }

    #[test]
    fn closed_wall_loop_in_any_order_is_a_closed_mitred_path() {
        let (mut m, l, _) = setup();
        let wt = m
            .add_wall_type(
                "W200",
                &[LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let mut w = |a, b| {
            m.add_wall(&WallSpec {
                a,
                b,
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap()
        };
        let a = w((0.0, 0.0), (6.0, 0.0));
        let b = w((6.0, 0.0), (6.0, 4.0));
        let c = w((6.0, 4.0), (0.0, 4.0));
        let d = w((0.0, 4.0), (0.0, 0.0));
        // selection in random order: the chain is ordered by shared ends
        let s = cornice(&mut m, vec![a, c, b, d], -1).unwrap();
        let p = &m.sweep_paths(s).unwrap()[0];
        assert!(p.closed);
        assert_eq!(p.pts.len(), 4);
        // the right face of a counter-clockwise traversal is outward: rectangle 6.2 × 4.2
        let q = m.sweep_quantities(s).unwrap();
        near(q.length, 2.0 * (6.2 + 4.2), 1e-9, "outer loop length");
        assert!(m.sweep_mesh(s).unwrap().is_watertight());
        // non-adjacent walls are an error, not a body across the room
        assert!(cornice(&mut m, vec![a, c], -1).is_err());
    }

    #[test]
    fn broken_join_target_degrades_instead_of_blocking_edits() {
        let (mut m, l, st) = setup();
        let deg = 0.5f64.atan().to_degrees();
        let main = m
            .add_roof(&footprint(
                l,
                st,
                rect(20.0, 10.0),
                vec![Some(deg), None, Some(deg), None],
                0.0,
            ))
            .unwrap();
        let ext = |a: (f64, f64), b: (f64, f64)| RoofSpec {
            level: l,
            slab_type: st,
            offset: 3.0,
            overhang: 0.0,
            fascia: 0.0,
            kind: RoofKind::Extrusion {
                a,
                b,
                profile: vec![(0.0, 0.0), (2.0, 2.0), (4.0, 0.0)],
                depth: 1.0,
            },
        };
        let wing = m.add_roof(&ext((8.0, -4.0), (12.0, -4.0))).unwrap();
        m.join_roof(wing, main).unwrap();
        // a join cycle is forbidden
        let e = m.join_roof(main, wing).unwrap_err();
        assert!(e.to_string().contains("cycle"), "{e}");
        // the target stopped building (level gone): the wing has no join, edits go through
        m.db.execute("UPDATE roof SET level=999 WHERE id=?1", [main])
            .unwrap();
        near(
            m.roof_quantities(wing).unwrap().geom.plan_area,
            4.0,
            1e-9,
            "unjoined",
        );
        m.add_level("L1", 3.0).unwrap();
        assert!(m
            .roof_edge_issues()
            .unwrap()
            .iter()
            .any(|i| i.table == "roof_ext" && i.id == wing));
        // editing the wing itself is not blocked by a broken target
        let mut ws = m.roof(wing).unwrap();
        ws.offset = 3.5;
        m.update_roof(wing, &ws).unwrap();
        m.undo().unwrap();
    }

    #[test]
    fn skylights_follow_moved_roof_span_faces_and_reject_overlaps() {
        let (mut m, l, st) = setup();
        let mut s = footprint(l, st, rect(10.0, 6.0), vec![Some(30.0); 4], 0.5);
        let id = m.add_roof(&s).unwrap();
        // across a hip edge is allowed
        let k = m
            .add_skylight(&SkylightSpec {
                roof: id,
                center: (2.0, 1.0),
                width: 3.0,
                length: 1.0,
                curb: 0.15,
            })
            .unwrap();
        // on top of the previous one is not allowed
        assert!(m
            .add_skylight(&SkylightSpec {
                center: (2.5, 1.0),
                width: 1.0,
                ..m.skylight(k).unwrap()
            })
            .is_err());
        let kinds: Vec<&str> = m
            .roof_edge_bodies(Some(l))
            .unwrap()
            .iter()
            .map(|b| b.0)
            .collect();
        assert!(kinds.contains(&"skylight") && kinds.contains(&"skylight_glass"));
        // moving a roof as a whole carries the skylight along
        if let RoofKind::Footprint { ring, .. } = &mut s.kind {
            for p in ring.iter_mut() {
                p.0 += 1.0;
            }
        }
        m.update_roof(id, &s).unwrap();
        assert_eq!(m.skylight(k).unwrap().center, (3.0, 1.0));
        assert!(m.roof_edge_issues().unwrap().is_empty());
        m.undo().unwrap();
        assert_eq!(m.skylight(k).unwrap().center, (2.0, 1.0));
        // editing a skylight past the eave is an error, within bounds one operation
        let mut e = m.skylight(k).unwrap();
        e.center = (5.0, -0.4);
        assert!(m.update_skylight(k, &e).is_err());
        e.center = (5.0, 1.0);
        e.width = 1.0;
        m.update_skylight(k, &e).unwrap();
        assert_eq!(m.skylight(k).unwrap().center, (5.0, 1.0));
    }

    #[test]
    fn soffit_sits_below_roof_body_and_fascia_area_not_double_counted() {
        let (mut m, l, st) = setup();
        let mut s = footprint(l, st, rect(10.0, 6.0), vec![Some(45.0); 4], 0.5);
        s.fascia = 0.2;
        let id = m.add_roof(&s).unwrap();
        let so = m.add_soffit(id, 0.0, 0.02, None).unwrap();
        let (mesh, _) = m.soffit_mesh(so).unwrap();
        let top = mesh.positions.iter().map(|p| p.z).fold(f64::MIN, f64::max);
        // soffit top is at the roof underside at the eave (t/cos 45°, not t)
        let low = m
            .roof_mesh(id)
            .unwrap()
            .positions
            .iter()
            .map(|p| p.z)
            .fold(f64::MAX, f64::min);
        assert!(
            (top - low).abs() < 1e-3,
            "soffit top {top} vs roof underside {low}"
        );
        let eave = m.roof_geometry(id).unwrap().eave_z;
        assert!(top < eave - 0.2 - 1e-3, "soffit top {top} inside roof body");
        assert!(m.roof_quantities(id).unwrap().fascia_area > 0.0);
        m.add_sweep(&SweepSpec {
            kind: SweepKind::Fascia,
            profile: "fascia_200".into(),
            host: SweepHost::Roof {
                roof: id,
                rakes: false,
            },
            side: -1,
            z: 0.0,
            dx: 0.0,
            dy: 0.0,
            material: None,
        })
        .unwrap();
        assert_eq!(m.roof_quantities(id).unwrap().fascia_area, 0.0);
    }

    #[test]
    fn vault_bulge_is_limited_and_bodies_cache_follows_edits() {
        let (mut m, l, st) = setup();
        let id = m
            .add_barrel_roof(l, st, 4.0, (0.0, 0.0), (6.0, 0.0), 2.4, 10.0)
            .unwrap();
        assert!(m.set_roof_arcs(id, &[-0.9], 0.0).is_err());
        assert!(m
            .add_barrel_roof(l, st, 4.0, (0.0, 0.0), (6.0, 0.0), 3.0, 10.0)
            .is_err());
        // the vault underside does not go below the springings by more than t/cos θ of the springing (|bulge| <= 0.8 -> θ <= 77.4°)
        let zmin = m
            .roof_mesh(id)
            .unwrap()
            .positions
            .iter()
            .map(|p| p.z)
            .fold(f64::MAX, f64::min);
        assert!(
            zmin > 4.0 - 0.2 / (2.0 * 0.8f64.atan()).cos() - 1e-6,
            "zmin {zmin}"
        );
        let n0 = m.roof_edge_bodies(None).unwrap().len();
        let k = m
            .add_skylight(&SkylightSpec {
                roof: id,
                center: (3.0, 5.0),
                width: 0.8,
                length: 1.0,
                curb: 0.1,
            })
            .unwrap();
        assert_eq!(
            m.roof_edge_bodies(None).unwrap().len(),
            n0 + 2,
            "skylight {k} on vault"
        );
        m.undo().unwrap();
        assert_eq!(m.roof_edge_bodies(None).unwrap().len(), n0);
    }
    #[test]
    fn taper_skips_slab_holes() {
        let (mut m, l, st) = setup();
        let slab = m
            .add_slab(&SlabSpec {
                level: l,
                slab_type: st,
                offset: 0.0,
                outer: rect(10.0, 10.0),
                holes: vec![vec![(4.0, 4.0), (6.0, 4.0), (6.0, 6.0), (4.0, 6.0)]],
            })
            .unwrap();
        let t = m.add_taper_on_slab(slab, 0.02, 0.05).unwrap();
        near(
            m.taper_quantities(t).unwrap().volume,
            96.0 * 0.05,
            1e-9,
            "flat with hole",
        );
        assert!(m.add_drain(t, (5.0, 5.0)).is_err(), "drain inside the hole");
        m.add_drain(t, (2.0, 2.0)).unwrap();
        let mesh = m.taper_mesh(t).unwrap();
        near(
            mesh.volume(),
            m.taper_quantities(t).unwrap().volume,
            1e-9,
            "mesh volume",
        );
    }
}
