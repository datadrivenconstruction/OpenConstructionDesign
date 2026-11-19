// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Framing (schema 26): braces between column/beam points, trusses as a single element (members
//! are built during regeneration), beam systems inside a contour (openings, reference edge,
//! layout rules). Ends: a brace ends at the column face it leaves through, or at the bottom/top
//! of the host beam; a system beam ends at the side face of a beam in its own height band + gap.

use std::collections::HashMap;

use ddc_geom::framing::{beam_lines_in, Truss};
use ddc_geom::member::{Cap, Just, Member, Path, Shape};
use ddc_geom::section::{Prism, Zfn};
use ddc_geom::{poly, v2, v3, V2, V3};
use rusqlite::{params, Connection, OptionalExtension};

pub use ddc_geom::framing::{Chord as TrussChord, Layout as BeamLayout, Web as TrussWeb};

use crate::ops::Inv;
use crate::struct_ext::Part;
use crate::structure::Built;
use crate::{Error, Model};

/// Host of a brace end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberHost {
    Column,
    Beam,
}

impl MemberHost {
    pub fn text(self) -> &'static str {
        match self {
            MemberHost::Column => "column",
            MemberHost::Beam => "beam",
        }
    }
    pub fn parse(s: &str) -> Option<MemberHost> {
        match s {
            "column" => Some(MemberHost::Column),
            "beam" => Some(MemberHost::Beam),
            _ => None,
        }
    }
    fn table(self) -> &'static str {
        match self {
            MemberHost::Column => "col",
            MemberHost::Beam => "beam",
        }
    }
}

/// Point on a member axis: t in [0, 1] from start to end (column: bottom to top).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub host: MemberHost,
    pub id: i64,
    pub t: f64,
}

/// Brace between two points; z is from the level. An end with an `Anchor` follows its host;
/// the coordinates are a fallback (host deleted).
#[derive(Clone, Debug, PartialEq)]
pub struct BraceSpec {
    pub level: i64,
    pub member_type: i64,
    pub a: (f64, f64, f64),
    pub b: (f64, f64, f64),
    pub start: Option<Anchor>,
    pub end: Option<Anchor>,
    pub rotation: f64,
}

/// Truss over the bottom chord axis a->b (z from the level).
#[derive(Clone, Debug, PartialEq)]
pub struct TrussSpec {
    pub level: i64,
    pub web: TrussWeb,
    pub chord: TrussChord,
    pub a: (f64, f64, f64),
    pub b: (f64, f64, f64),
    pub height: f64,
    pub heel: f64,
    pub panels: usize,
    pub chord_type: i64,
    pub web_type: i64,
}

/// Beam system: beams of type `member_type` inside a contour, spaced along direction `angle`.
#[derive(Clone, Debug, PartialEq)]
pub struct BeamSystemSpec {
    pub level: i64,
    pub member_type: i64,
    pub ring: Vec<(f64, f64)>,
    /// Openings (inner contours): beams are interrupted.
    pub holes: Vec<Vec<(f64, f64)>>,
    /// Contour edge `ring[i] -> ring[i+1]` from which the layout is measured; None = the side
    /// to the right of the beam direction.
    pub start_edge: Option<usize>,
    /// Elevation of the beam reference line from the level.
    pub z: f64,
    pub angle: f64,
    pub spacing: f64,
    pub layout: BeamLayout,
    /// Gap at each end after trimming against the contour beams.
    pub end_gap: f64,
    pub zjust: Just,
}

fn invalid<T>(m: String) -> Result<T, Error> {
    Err(Error::Invalid(m))
}

fn count(db: &Connection, table: &str, id: i64) -> Result<i64, Error> {
    Ok(db.query_row(
        &format!("SELECT count(*) FROM {table} WHERE id=?1"),
        [id],
        |r| r.get(0),
    )?)
}

fn need(db: &Connection, table: &str, id: i64, what: &str) -> Result<(), Error> {
    if count(db, table, id)? == 0 {
        return invalid(format!("{what}: no such {table} {id}"));
    }
    Ok(())
}

fn fin(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite())
}

/// The member type exists and its section can be read (otherwise the element is not built).
fn need_shape(db: &Connection, id: i64, what: &str) -> Result<Shape, Error> {
    need(db, "member_type", id, what)?;
    match member_types(db)?.remove(&id) {
        Some((sh, _)) => Ok(sh),
        None => invalid(format!("{what}: member type {id} has no readable section")),
    }
}

fn check_brace(db: &Connection, s: &BraceSpec) -> Result<(), Error> {
    need(db, "level", s.level, "brace")?;
    need_shape(db, s.member_type, "brace")?;
    let (a, b) = (s.a, s.b);
    if !fin(&[a.0, a.1, a.2, b.0, b.1, b.2, s.rotation]) {
        return invalid("brace: non-finite".into());
    }
    let d = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2) + (b.2 - a.2).powi(2)).sqrt();
    if d < 1e-6 {
        return Err(Error::Degenerate("brace length would be zero".into()));
    }
    for h in [s.start, s.end].into_iter().flatten() {
        need(db, h.host.table(), h.id, "brace host")?;
        if !(0.0..=1.0).contains(&h.t) {
            return invalid(format!("brace host t {} outside 0..1", h.t));
        }
    }
    Ok(())
}

fn check_truss(db: &Connection, s: &TrussSpec) -> Result<(), Error> {
    need(db, "level", s.level, "truss")?;
    need_shape(db, s.chord_type, "truss chord")?;
    need_shape(db, s.web_type, "truss web")?;
    truss_geom(s, 0.0).check().map_err(Error::Invalid)
}

fn pts(r: &[(f64, f64)]) -> Vec<V2> {
    r.iter().map(|p| v2(p.0, p.1)).collect()
}

/// Check against the layout after trimming by the current beams: a system without a single beam
/// is rejected (otherwise it would stay "without quantities" forever).
fn check_system(db: &Connection, s: &BeamSystemSpec) -> Result<(), Error> {
    need(db, "level", s.level, "beam system")?;
    let sh = need_shape(db, s.member_type, "beam system")?;
    let ring = pts(&s.ring);
    let fixed_count = matches!(s.layout, BeamLayout::FixedCount(_));
    let ok = fin(&[s.z, s.angle, s.spacing, s.end_gap])
        && ring.iter().all(|p| fin(&[p.x, p.y]))
        && (s.spacing > 0.0 || fixed_count)
        && s.end_gap >= 0.0
        && s.layout != BeamLayout::FixedCount(0)
        && s.start_edge.is_none_or(|i| i < s.ring.len());
    if !ok || ring.len() < 3 || poly::simple_ring(&ring, 1e-9).is_err() {
        return invalid(
            "beam system: simple outline, spacing > 0 (or a beam count), gap >= 0".into(),
        );
    }
    for h in &s.holes {
        let h = pts(h);
        let inside = h
            .iter()
            .all(|p| ddc_geom::predicates::point_in_ring(*p, &ring) > 0);
        if h.len() < 3 || !inside || poly::simple_ring(&h, 1e-9).is_err() {
            return invalid("beam system: openings must be simple and inside the outline".into());
        }
    }
    let holes: Vec<Vec<V2>> = s.holes.iter().map(|h| pts(h)).collect();
    if beam_lines_in(&ring, &holes, s.angle, s.spacing, s.layout, s.start_edge).is_empty() {
        return invalid("beam system: no beam fits the outline at this spacing".into());
    }
    let members = crate::structure::build_all(db)?;
    let elev = levels(db)?.get(&s.level).copied().unwrap_or(0.0);
    if joists(s, &sh, elev, &girders(&members)).is_empty() {
        return invalid("beam system: every beam is trimmed away by the girders".into());
    }
    Ok(())
}

fn truss_geom(s: &TrussSpec, elev: f64) -> Truss {
    Truss {
        a: v3(s.a.0, s.a.1, elev + s.a.2),
        b: v3(s.b.0, s.b.1, elev + s.b.2),
        height: s.height,
        heel: s.heel,
        panels: s.panels,
        web: s.web,
        chord: s.chord,
    }
}

const BRACE_SEL: &str =
    "SELECT id,level,mtype,ax,ay,az,bx,by,bz,host_a,ref_a,t_a,host_b,ref_b,t_b,rotation FROM brace";
const TRUSS_SEL: &str =
    "SELECT id,level,web,chord,ax,ay,az,bx,by,bz,height,heel,panels,chord_type,web_type FROM truss";

fn anchor(h: Option<String>, id: Option<i64>, t: Option<f64>) -> Option<Anchor> {
    Some(Anchor {
        host: MemberHost::parse(&h?)?,
        id: id?,
        t: t?,
    })
}

fn brace_row(r: &rusqlite::Row) -> rusqlite::Result<(i64, BraceSpec)> {
    Ok((
        r.get(0)?,
        BraceSpec {
            level: r.get(1)?,
            member_type: r.get(2)?,
            a: (r.get(3)?, r.get(4)?, r.get(5)?),
            b: (r.get(6)?, r.get(7)?, r.get(8)?),
            start: anchor(r.get(9)?, r.get(10)?, r.get(11)?),
            end: anchor(r.get(12)?, r.get(13)?, r.get(14)?),
            rotation: r.get(15)?,
        },
    ))
}

fn truss_row(r: &rusqlite::Row) -> rusqlite::Result<(i64, TrussSpec)> {
    Ok((
        r.get(0)?,
        TrussSpec {
            level: r.get(1)?,
            web: TrussWeb::parse(&r.get::<_, String>(2)?),
            chord: TrussChord::parse(&r.get::<_, String>(3)?),
            a: (r.get(4)?, r.get(5)?, r.get(6)?),
            b: (r.get(7)?, r.get(8)?, r.get(9)?),
            height: r.get(10)?,
            heel: r.get(11)?,
            panels: r.get::<_, i64>(12)?.max(0) as usize,
            chord_type: r.get(13)?,
            web_type: r.get(14)?,
        },
    ))
}

fn system_spec(db: &Connection, id: i64) -> Result<BeamSystemSpec, Error> {
    let mut s = db
        .query_row(
            "SELECT level,mtype,z,angle,spacing,rule,end_gap,zjust,start_edge,count \
             FROM beam_system WHERE id=?1",
            [id],
            |r| {
                let count: i64 = r.get(9)?;
                let start: Option<i64> = r.get(8)?;
                Ok(BeamSystemSpec {
                    level: r.get(0)?,
                    member_type: r.get(1)?,
                    ring: vec![],
                    holes: vec![],
                    start_edge: start.and_then(|i| usize::try_from(i).ok()),
                    z: r.get(2)?,
                    angle: r.get(3)?,
                    spacing: r.get(4)?,
                    layout: BeamLayout::parse_with(&r.get::<_, String>(5)?, count.max(0) as usize),
                    end_gap: r.get(6)?,
                    zjust: Just::from_i(r.get(7)?),
                })
            },
        )
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no such beam system {id}")))?;
    let mut st =
        db.prepare("SELECT ring, x, y FROM beam_system_vertex WHERE sys=?1 ORDER BY idx")?;
    let rows: Vec<(i64, f64, f64)> = st
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (ring, x, y) in rows {
        if ring <= 0 {
            s.ring.push((x, y));
        } else {
            let k = ring as usize - 1;
            if s.holes.len() <= k {
                s.holes.resize(k + 1, vec![]);
            }
            s.holes[k].push((x, y));
        }
    }
    s.holes.retain(|h| !h.is_empty());
    Ok(s)
}

fn insert_brace(db: &Connection, s: &BraceSpec, id: Option<i64>) -> Result<i64, Error> {
    let h = |a: Option<Anchor>| (a.map(|a| a.host.text()), a.map(|a| a.id), a.map(|a| a.t));
    let (ha, ra, ta) = h(s.start);
    let (hb, rb, tb) = h(s.end);
    db.execute(
        "INSERT OR REPLACE INTO brace(id,level,mtype,ax,ay,az,bx,by,bz,host_a,ref_a,t_a,host_b,ref_b,t_b,rotation) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        params![
            id,
            s.level,
            s.member_type,
            s.a.0,
            s.a.1,
            s.a.2,
            s.b.0,
            s.b.1,
            s.b.2,
            ha,
            ra,
            ta,
            hb,
            rb,
            tb,
            s.rotation
        ],
    )?;
    Ok(db.last_insert_rowid())
}

fn insert_truss(db: &Connection, s: &TrussSpec, id: Option<i64>) -> Result<i64, Error> {
    db.execute(
        "INSERT OR REPLACE INTO truss(id,level,web,chord,ax,ay,az,bx,by,bz,height,heel,panels,chord_type,web_type) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            id,
            s.level,
            s.web.text(),
            s.chord.text(),
            s.a.0,
            s.a.1,
            s.a.2,
            s.b.0,
            s.b.1,
            s.b.2,
            s.height,
            s.heel,
            s.panels as i64,
            s.chord_type,
            s.web_type
        ],
    )?;
    Ok(db.last_insert_rowid())
}

fn insert_system(db: &Connection, s: &BeamSystemSpec, id: Option<i64>) -> Result<i64, Error> {
    db.execute(
        "INSERT OR REPLACE INTO beam_system(id,level,mtype,z,angle,spacing,rule,end_gap,zjust,\
         start_edge,count) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            id,
            s.level,
            s.member_type,
            s.z,
            s.angle,
            s.spacing,
            s.layout.text(),
            s.end_gap,
            s.zjust.to_i(),
            s.start_edge.map(|i| i as i64),
            s.layout.count() as i64
        ],
    )?;
    let sid = db.last_insert_rowid();
    db.execute("DELETE FROM beam_system_vertex WHERE sys=?1", [sid])?;
    let rings = std::iter::once(&s.ring).chain(&s.holes);
    let mut idx = 0i64;
    for (k, r) in rings.enumerate() {
        for p in r {
            db.execute(
                "INSERT INTO beam_system_vertex(sys, idx, x, y, ring) VALUES (?1,?2,?3,?4,?5)",
                params![sid, idx, p.0, p.1, k as i64],
            )?;
            idx += 1;
        }
    }
    Ok(sid)
}

fn member_types(db: &Connection) -> Result<HashMap<i64, (Shape, String)>, Error> {
    let mut st = db.prepare("SELECT id, shape, material FROM member_type")?;
    let rows: Vec<(i64, String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, s, m)| Shape::parse(&s).map(|sh| (id, (sh, m))))
        .collect())
}

fn flat(p: V3) -> V2 {
    v2(p.x, p.y)
}

/// Columns and beams by (kind, id): O(1) host lookup.
type Index<'a> = HashMap<(&'static str, i64), &'a Built>;

fn index(members: &[Built]) -> Index<'_> {
    members.iter().map(|b| ((b.kind, b.id), b)).collect()
}

fn find<'a>(ix: &Index<'a>, host: MemberHost, id: i64) -> Option<&'a Built> {
    ix.get(&(host.text(), id)).copied()
}

/// World point of an anchor on the host axis (straight axes).
fn anchor_point(ix: &Index, a: &Anchor) -> Option<V3> {
    let (p, q) = find(ix, a.host, a.id)?.m.axis_ends()?;
    Some(p + (q - p) * a.t)
}

/// Outward normal of the contour edge (counter-clockwise) nearest to point `q`.
fn edge_normal(ring: &[V2], q: V2) -> Option<V2> {
    let mut best: Option<(f64, V2)> = None;
    for k in 0..ring.len() {
        let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
        let e = b - a;
        let l2 = e.dot(e);
        if l2 < 1e-24 {
            continue;
        }
        let t = ((q - a).dot(e) / l2).clamp(0.0, 1.0);
        let d = (a + e * t - q).len();
        if best.is_none_or(|x| d < x.0) {
            best = Some((d, v2(e.y, -e.x) * (1.0 / l2.sqrt())));
        }
    }
    best.map(|b| b.1)
}

/// Brace end at its host: for a column, the section face plane the brace leaves through;
/// for a horizontal beam, its bottom or top (on the side of the other end).
fn host_cap(ix: &Index, a: &Anchor, p: V3, other: V3) -> Option<Cap> {
    let b = find(ix, a.host, a.id)?;
    match a.host {
        MemberHost::Column => {
            let ring = b.m.plan_section(p.z)?;
            let iv = poly::inside_intervals(flat(p), flat(other), std::slice::from_ref(&ring));
            let t = iv.iter().find(|w| w.0 <= 1e-9).map(|w| w.1)?;
            let h = v3(other.x - p.x, other.y - p.y, 0.0);
            if h.len() < 1e-9 || t >= 1.0 - 1e-9 {
                return None;
            }
            let q = p + (other - p) * t;
            let hd = h.normalized();
            // exit face; for a nearly tangent exit, a plane perpendicular to the brace plan
            let face = edge_normal(&ring, flat(q))
                .map(|n| v3(n.x, n.y, 0.0))
                .filter(|n| n.dot(hd) > 0.1)
                .unwrap_or(hd);
            // end normal points outward from the body, toward the host
            Some(Cap { p: q, n: -face })
        }
        MemberHost::Beam => {
            let u = b.m.axis_dir()?;
            if u.z.abs() > 1e-9 || b.m.rotation.abs() > 1e-12 || (other.z - p.z).abs() < 1e-6 {
                return None;
            }
            let (lo, hi) = b.m.extents();
            let (z, nz) = if other.z < p.z {
                (p.z + lo.y, 1.0)
            } else {
                (p.z + hi.y, -1.0)
            };
            Some(Cap {
                p: v3(p.x, p.y, z),
                n: v3(0.0, 0.0, nz),
            })
        }
    }
}

fn levels(db: &Connection) -> Result<HashMap<i64, f64>, Error> {
    Ok(crate::walls::levels(db)?.into_iter().collect())
}

/// Cut prism of a straight member: the footprint is the plan hull, the bottom and top are
/// inclined planes along the axis (horizontal ones for a vertical member).
fn bar_prism(m: &Member, mesh: &ddc_geom::mesh::Mesh, plan: &[V2]) -> Option<Prism> {
    if plan.len() < 3 || mesh.positions.is_empty() {
        return None;
    }
    let (a, b) = m.axis_ends()?;
    let e = flat(b) - flat(a);
    let l = e.len();
    let (dir, slope) = if l < 1e-6 {
        (v2(1.0, 0.0), 0.0)
    } else {
        (e * (1.0 / l), (b.z - a.z) / l)
    };
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for p in &mesh.positions {
        let r = p.z - a.z - slope * (flat(*p) - flat(a)).dot(dir);
        (lo, hi) = (lo.min(r), hi.max(r));
    }
    let z = |dz: f64| Zfn {
        o: flat(a),
        z: a.z + dz,
        grad: dir * slope,
        dir,
        prof: vec![],
    };
    Some(Prism {
        rings: vec![plan.to_vec()],
        bottom: z(lo),
        top: z(hi),
        voids: vec![],
    })
}

pub(crate) fn mpart(
    kind: &'static str,
    id: i64,
    level: i64,
    k: i64,
    role: &'static str,
    mat: &str,
    m: &Member,
) -> Part {
    let q = m.quantities();
    let mesh = m.mesh();
    let xy: Vec<V2> = mesh.positions.iter().map(|p| flat(*p)).collect();
    let plan = crate::struct_ext::hull(&xy);
    let (w, h) = m.shape.size();
    let mut p = Part::new(kind, id, level, role, mat);
    p.part = k;
    p.length = q.length;
    p.volume = q.volume;
    p.area = q.surface;
    p.size = [q.length, w, h];
    p.prisms.extend(bar_prism(m, &mesh, &plan));
    if plan.len() >= 3 {
        p.plan = vec![plan];
    }
    p.meshes = vec![mesh];
    p
}

fn line_member(shape: &Shape, a: V3, b: V3, rotation: f64, zjust: Just) -> Member {
    Member {
        shape: shape.clone(),
        path: Path::Line { a, b },
        rotation,
        yjust: Just::Center,
        zjust,
        column: false,
        start: None,
        end: None,
    }
}

fn girders(members: &[Built]) -> Vec<&Built> {
    members.iter().filter(|b| b.kind == "beam").collect()
}

/// Trims a system beam end at point `p` against the beams whose body (plan, height band
/// `band`) contains it: up to the side face, accounting for the alignment. `d` points into the beam.
fn boundary_trim(beams: &[&Built], p: V2, d: V2, band: (f64, f64)) -> f64 {
    let mut trim: f64 = 0.0;
    for b in beams {
        let Some((s3, e3)) = b.m.axis_ends() else {
            continue;
        };
        let (s, e) = (flat(s3), flat(e3));
        let l = (e - s).len();
        if l < 1e-9 {
            continue;
        }
        let u = (e - s) * (1.0 / l);
        let nrm = v2(-u.y, u.x);
        let along = (p - s).dot(u);
        if along < -1e-6 || along > l + 1e-6 {
            continue;
        }
        // transverse and vertical limits of the body: a rotated section uses its circumscribed circle
        let (lo, hi) = b.m.extents();
        let r = {
            let (w, h) = b.m.shape.size();
            (w * w + h * h).sqrt() / 2.0
        };
        let flat_sec = b.m.rotation.abs() < 1e-12;
        let (y0, y1) = if flat_sec { (lo.x, hi.x) } else { (-r, r) };
        let off = (p - s).dot(nrm);
        if off < y0 - 1e-3 || off > y1 + 1e-3 {
            continue;
        }
        let zc = s3.z + (e3.z - s3.z) * (along / l).clamp(0.0, 1.0);
        let (z0, z1) = if flat_sec {
            (zc + lo.y, zc + hi.y)
        } else {
            (zc - r, zc + r)
        };
        if z1.min(band.1) - z0.max(band.0) <= 1e-6 {
            continue;
        }
        let rate = d.dot(nrm);
        if rate.abs() < 0.1 {
            continue;
        }
        let t = if rate > 0.0 {
            (y1 - off) / rate
        } else {
            (off - y0) / -rate
        };
        trim = trim.max(t);
    }
    trim
}

/// System beams after trimming against the contour beams and the gap.
fn joists(s: &BeamSystemSpec, sh: &Shape, elev: f64, beams: &[&Built]) -> Vec<Member> {
    let z = elev + s.z;
    let ring = pts(&s.ring);
    let holes: Vec<Vec<V2>> = s.holes.iter().map(|h| pts(h)).collect();
    let d = v2(s.angle.cos(), s.angle.sin());
    let band = {
        let probe = line_member(sh, v3(0.0, 0.0, z), v3(1.0, 0.0, z), 0.0, s.zjust);
        let (lo, hi) = probe.extents();
        (z + lo.y, z + hi.y)
    };
    let mut out = vec![];
    for (p, q) in beam_lines_in(&ring, &holes, s.angle, s.spacing, s.layout, s.start_edge) {
        let ta = boundary_trim(beams, p, d, band) + s.end_gap;
        let tb = boundary_trim(beams, q, -d, band) + s.end_gap;
        let (p2, q2) = (p + d * ta, q - d * tb);
        if (q2 - p2).dot(d) <= 1e-3 {
            continue;
        }
        out.push(line_member(
            sh,
            v3(p2.x, p2.y, z),
            v3(q2.x, q2.y, z),
            0.0,
            s.zjust,
        ));
    }
    out
}

/// Fallback brace ends = the anchor points on hosts now; if a host is deleted, the end is
/// detached and stays in place (column and beam ids are reused).
pub(crate) fn sync_hosts(db: &Connection, members: &[Built]) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    let n: i64 = db.query_row(
        "SELECT count(*) FROM brace WHERE host_a IS NOT NULL OR host_b IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Ok(inv);
    }
    let lv = levels(db)?;
    let ix = index(members);
    let mut st = db.prepare(&format!(
        "{BRACE_SEL} WHERE host_a IS NOT NULL OR host_b IS NOT NULL ORDER BY id"
    ))?;
    let rows: Vec<(i64, BraceSpec)> = st.query_map([], brace_row)?.collect::<Result<_, _>>()?;
    for (id, s) in rows {
        let e = lv.get(&s.level).copied().unwrap_or(0.0);
        let mut sql: Vec<(String, Vec<f64>)> = vec![];
        for (end, anchor, now) in [("a", s.start, s.a), ("b", s.end, s.b)] {
            let Some(an) = anchor else { continue };
            if find(&ix, an.host, an.id).is_none() {
                let q = format!(
                    "UPDATE brace SET host_{end}=NULL, ref_{end}=NULL, t_{end}=NULL WHERE id=?1"
                );
                sql.push((q, vec![]));
                continue;
            }
            let Some(p) = anchor_point(&ix, &an) else {
                continue;
            };
            let q = [p.x, p.y, p.z - e];
            if [now.0, now.1, now.2]
                .iter()
                .zip(&q)
                .any(|(x, y)| (x - y).abs() > 1e-12)
            {
                let u = format!("UPDATE brace SET {end}x=?2, {end}y=?3, {end}z=?4 WHERE id=?1");
                sql.push((u, q.to_vec()));
            }
        }
        if sql.is_empty() {
            continue;
        }
        inv = inv.restore(db, "brace", "id", id)?;
        for (q, v) in sql {
            match v.as_slice() {
                [x, y, z] => db.execute(&q, params![id, x, y, z])?,
                _ => db.execute(&q, [id])?,
            };
        }
    }
    Ok(inv)
}

/// Braces as straight members between their own end points (no host caps): what a steel
/// connection joins.
pub(crate) fn brace_members(db: &Connection) -> Result<Vec<Built>, Error> {
    let lv = levels(db)?;
    let ty = member_types(db)?;
    let mut st = db.prepare(&format!("{BRACE_SEL} ORDER BY id"))?;
    let braces: Vec<(i64, BraceSpec)> = st.query_map([], brace_row)?.collect::<Result<_, _>>()?;
    Ok(braces
        .into_iter()
        .filter_map(|(id, s)| {
            let (sh, _) = ty.get(&s.member_type)?;
            let e = lv.get(&s.level).copied().unwrap_or(0.0);
            let (a, b) = (v3(s.a.0, s.a.1, e + s.a.2), v3(s.b.0, s.b.1, e + s.b.2));
            ((b - a).len() > 1e-6).then(|| Built {
                kind: "brace",
                id,
                level: s.level,
                m: line_member(sh, a, b, s.rotation, Just::Center),
                cut: 0.0,
                structural: true,
            })
        })
        .collect())
}

pub(crate) fn build(db: &Connection, members: &[Built]) -> Result<Vec<Part>, Error> {
    let lv = levels(db)?;
    let ty = member_types(db)?;
    let ix = index(members);
    let beams = girders(members);
    let mut out = vec![];
    let mut st = db.prepare(&format!("{BRACE_SEL} ORDER BY id"))?;
    let braces: Vec<(i64, BraceSpec)> = st.query_map([], brace_row)?.collect::<Result<_, _>>()?;
    for (id, s) in braces {
        let Some((sh, mat)) = ty.get(&s.member_type) else {
            continue;
        };
        let e = lv.get(&s.level).copied().unwrap_or(0.0);
        let free = |p: (f64, f64, f64)| v3(p.0, p.1, e + p.2);
        let pa = s
            .start
            .and_then(|a| anchor_point(&ix, &a))
            .unwrap_or(free(s.a));
        let pb = s
            .end
            .and_then(|a| anchor_point(&ix, &a))
            .unwrap_or(free(s.b));
        if (pb - pa).len() < 1e-6 {
            continue;
        }
        let mut m = line_member(sh, pa, pb, s.rotation, Just::Center);
        let plain = m.clone();
        m.start = s.start.and_then(|a| host_cap(&ix, &a, pa, pb));
        m.end = s.end.and_then(|a| host_cap(&ix, &a, pb, pa));
        if m.quantities().length <= 1e-6 {
            m = plain;
        }
        out.push(mpart("brace", id, s.level, 0, "brace", mat, &m));
    }
    let mut st = db.prepare(&format!("{TRUSS_SEL} ORDER BY id"))?;
    let trusses: Vec<(i64, TrussSpec)> = st.query_map([], truss_row)?.collect::<Result<_, _>>()?;
    for (id, s) in trusses {
        let (Some((cs, cm)), Some((ws, wm))) = (ty.get(&s.chord_type), ty.get(&s.web_type)) else {
            continue;
        };
        let t = truss_geom(&s, lv.get(&s.level).copied().unwrap_or(0.0));
        if t.check().is_err() {
            continue;
        }
        for (k, (role, m)) in t.members(cs, ws).into_iter().enumerate() {
            let mat = if role.web() { wm } else { cm };
            out.push(mpart("truss", id, s.level, k as i64, role.text(), mat, &m));
        }
    }
    let ids: Vec<i64> = {
        let mut st = db.prepare("SELECT id FROM beam_system ORDER BY id")?;
        let r = st.query_map([], |r| r.get(0))?;
        r.collect::<Result<_, _>>()?
    };
    for id in ids {
        let s = system_spec(db, id)?;
        let Some((sh, mat)) = ty.get(&s.member_type) else {
            continue;
        };
        let elev = lv.get(&s.level).copied().unwrap_or(0.0);
        for (k, m) in joists(&s, sh, elev, &beams).iter().enumerate() {
            out.push(mpart("beam_system", id, s.level, k as i64, "joist", mat, m));
        }
    }
    Ok(out)
}

impl Model {
    pub fn add_brace(&mut self, s: &BraceSpec) -> Result<i64, Error> {
        Ok(self.add_braces(std::slice::from_ref(s))?[0])
    }

    /// Several braces in one operation (a cross = two braces, one undo).
    pub fn add_braces(&mut self, ss: &[BraceSpec]) -> Result<Vec<i64>, Error> {
        for s in ss {
            check_brace(&self.db, s)?;
        }
        self.op("member_brace_add", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            for s in ss {
                let id = insert_brace(db, s, None)?;
                inv = inv.delete("brace", "id", id);
                ids.push(id);
            }
            let first = ids.first().copied().unwrap_or(0);
            Ok((ids, first, inv))
        })
    }

    pub fn update_brace(&mut self, id: i64, s: &BraceSpec) -> Result<(), Error> {
        check_brace(&self.db, s)?;
        self.op("member_brace_edit", |db| {
            if count(db, "brace", id)? == 0 {
                return invalid(format!("no such brace {id}"));
            }
            let inv = Inv::default().restore(db, "brace", "id", id)?;
            insert_brace(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn brace(&self, id: i64) -> Result<BraceSpec, Error> {
        Ok(self
            .db
            .query_row(&format!("{BRACE_SEL} WHERE id=?1"), [id], brace_row)?
            .1)
    }

    /// Brace anchor under plan point `at` at height `z` from the level: a column axis (the point is inside
    /// the section or within `radius` of the axis) or a straight beam (axis within `radius`). Columns come first.
    pub fn brace_anchor(
        &self,
        level: i64,
        at: (f64, f64),
        z: f64,
        radius: f64,
    ) -> Result<Option<Anchor>, Error> {
        let zw = levels(&self.db)?.get(&level).copied().unwrap_or(0.0) + z;
        let built = self.struct_build()?;
        let members = &built.members;
        let p = v2(at.0, at.1);
        let mut best: Option<(f64, Anchor)> = None;
        let offer = |d: f64, a: Anchor, best: &mut Option<(f64, Anchor)>| {
            if best.is_none_or(|b| d < b.0) {
                *best = Some((d, a));
            }
        };
        for b in members {
            let Some((s, e)) = b.m.axis_ends() else {
                continue;
            };
            if b.kind == "column" {
                let (z0, z1) = (s.z.min(e.z), s.z.max(e.z));
                if zw < z0 - 1e-9 || zw > z1 + 1e-9 || (e.z - s.z).abs() < 1e-9 {
                    continue;
                }
                let t = (zw - s.z) / (e.z - s.z);
                let c = s + (e - s) * t;
                let d = (flat(c) - p).len();
                let inside =
                    b.m.plan_section(zw)
                        .is_some_and(|r| ddc_geom::predicates::point_in_ring(p, &r) >= 0);
                if inside || d <= radius {
                    let a = Anchor {
                        host: MemberHost::Column,
                        id: b.id,
                        t: t.clamp(0.0, 1.0),
                    };
                    offer(d - 1e6, a, &mut best);
                }
            } else if b.kind == "beam" {
                let (fs, fe) = (flat(s), flat(e));
                let l = (fe - fs).len();
                if l < 1e-9 {
                    continue;
                }
                let u = (fe - fs) * (1.0 / l);
                let along = ((p - fs).dot(u) / l).clamp(0.0, 1.0);
                let c = s + (e - s) * along;
                let d = (flat(c) - p).len();
                let h = b.m.shape.size().1.max(radius);
                if d <= radius && (c.z - zw).abs() <= h {
                    let a = Anchor {
                        host: MemberHost::Beam,
                        id: b.id,
                        t: along,
                    };
                    offer(d, a, &mut best);
                }
            }
        }
        Ok(best.map(|b| b.1))
    }

    pub fn add_truss(&mut self, s: &TrussSpec) -> Result<i64, Error> {
        check_truss(&self.db, s)?;
        self.op("member_truss_add", |db| {
            let id = insert_truss(db, s, None)?;
            Ok((id, id, Inv::default().delete("truss", "id", id)))
        })
    }

    pub fn update_truss(&mut self, id: i64, s: &TrussSpec) -> Result<(), Error> {
        check_truss(&self.db, s)?;
        self.op("member_truss_edit", |db| {
            if count(db, "truss", id)? == 0 {
                return invalid(format!("no such truss {id}"));
            }
            let inv = Inv::default().restore(db, "truss", "id", id)?;
            insert_truss(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn truss(&self, id: i64) -> Result<TrussSpec, Error> {
        Ok(self
            .db
            .query_row(&format!("{TRUSS_SEL} WHERE id=?1"), [id], truss_row)?
            .1)
    }

    pub fn add_beam_system(&mut self, s: &BeamSystemSpec) -> Result<i64, Error> {
        check_system(&self.db, s)?;
        self.op("member_system_add", |db| {
            let id = insert_system(db, s, None)?;
            let inv = Inv::default()
                .delete("beam_system_vertex", "sys", id)
                .delete("beam_system", "id", id);
            Ok((id, id, inv))
        })
    }

    pub fn update_beam_system(&mut self, id: i64, s: &BeamSystemSpec) -> Result<(), Error> {
        check_system(&self.db, s)?;
        self.op("member_system_edit", |db| {
            if count(db, "beam_system", id)? == 0 {
                return invalid(format!("no such beam system {id}"));
            }
            let inv = Inv::default()
                .delete("beam_system_vertex", "sys", id)
                .restore(db, "beam_system", "id", id)?
                .restore(db, "beam_system_vertex", "sys", id)?;
            insert_system(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn beam_system(&self, id: i64) -> Result<BeamSystemSpec, Error> {
        system_spec(&self.db, id)
    }

    /// Joists the system actually builds now (layout, holes, girder trims), not the stored count.
    pub fn beam_system_joist_count(&self, id: i64) -> Result<usize, Error> {
        let s = system_spec(&self.db, id)?;
        let ty = member_types(&self.db)?;
        let Some((sh, _)) = ty.get(&s.member_type) else {
            return Ok(0);
        };
        let elev = levels(&self.db)?.get(&s.level).copied().unwrap_or(0.0);
        let members = crate::structure::build_all(&self.db)?;
        Ok(joists(&s, sh, elev, &girders(&members)).len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BeamJoin, BeamSpec, ColumnSpec};

    fn near(a: f64, b: f64, what: &str) {
        assert!((a - b).abs() < 1e-6, "{what}: {a} vs {b}");
    }

    fn setup() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let l1 = m.add_level("L1", 3.0).unwrap();
        (m, l0, l1)
    }

    fn rect(m: &mut Model, name: &str, b: f64, h: f64) -> i64 {
        m.add_member_type(name, &Shape::Rect { b, h }, "Steel")
            .unwrap()
    }

    fn role_len(m: &Model, id: i64, role: &str) -> f64 {
        m.struct_parts("truss", id)
            .unwrap()
            .iter()
            .filter(|p| p.0 == role)
            .map(|p| p.1)
            .sum()
    }

    fn truss_spec(level: i64, ct: i64, wt: i64) -> TrussSpec {
        TrussSpec {
            level,
            web: TrussWeb::Pratt,
            chord: TrussChord::Flat,
            a: (0.0, 0.0, 3.0),
            b: (12.0, 0.0, 3.0),
            height: 2.0,
            heel: 0.0,
            panels: 4,
            chord_type: ct,
            web_type: wt,
        }
    }

    #[test]
    fn truss_members_quantities_weight_and_undo() {
        let (mut m, l0, _) = setup();
        let ct = rect(&mut m, "R 100x200", 0.1, 0.2);
        let wt = rect(&mut m, "R 80x80", 0.08, 0.08);
        let id = m.add_truss(&truss_spec(l0, ct, wt)).unwrap();
        // 4 + 4 chords, 5 posts, 4 diagonals
        assert_eq!(m.struct_parts("truss", id).unwrap().len(), 17);
        near(role_len(&m, id, "bottom"), 12.0, "bottom chord");
        near(role_len(&m, id, "top"), 12.0, "top chord");
        near(
            role_len(&m, id, "vertical"),
            5.0 * 1.8,
            "posts cut back to chord faces",
        );
        let q = m.struct_quantities("truss", id).unwrap();
        near(q.weight.unwrap(), q.volume * 7850.0, "steel weight");
        let mut s = truss_spec(l0, ct, wt);
        s.panels = 6;
        s.web = TrussWeb::Warren;
        m.update_truss(id, &s).unwrap();
        assert_eq!(m.truss(id).unwrap(), s);
        assert_eq!(m.struct_parts("truss", id).unwrap().len(), 6 + 6 + 2 + 6);
        m.undo().unwrap();
        assert_eq!(m.struct_parts("truss", id).unwrap().len(), 17);
        m.undo().unwrap();
        assert!(m.struct_parts("truss", id).unwrap().is_empty());
        let mut odd = truss_spec(l0, ct, wt);
        odd.chord = TrussChord::Pitched;
        odd.panels = 5;
        assert!(m.add_truss(&odd).is_err());
    }

    fn col(m: &mut Model, l0: i64, l1: i64, t: i64, at: (f64, f64)) -> i64 {
        m.add_column(&ColumnSpec {
            level: l0,
            top_level: Some(l1),
            member_type: t,
            structural: true,
            at,
            base_offset: 0.0,
            top_offset: 0.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap()
    }

    #[test]
    fn brace_between_columns_trims_to_faces_and_follows() {
        let (mut m, l0, l1) = setup();
        let ct = rect(&mut m, "R 400x400", 0.4, 0.4);
        let c1 = col(&mut m, l0, l1, ct, (0.0, 0.0));
        let c2 = col(&mut m, l0, l1, ct, (6.0, 0.0));
        let bt = rect(&mut m, "R 100x100", 0.1, 0.1);
        let a = m.brace_anchor(l0, (0.05, 0.0), 0.3, 0.2).unwrap().unwrap();
        assert_eq!((a.host, a.id), (MemberHost::Column, c1));
        near(a.t, 0.1, "anchor height");
        let s = BraceSpec {
            level: l0,
            member_type: bt,
            a: (0.0, 0.0, 0.3),
            b: (6.0, 0.0, 2.7),
            start: Some(a),
            end: Some(Anchor {
                host: MemberHost::Column,
                id: c2,
                t: 0.9,
            }),
            rotation: 0.0,
        };
        let ids = m.add_braces(&[s.clone(), s.clone()]).unwrap();
        assert_eq!(ids.len(), 2);
        let len = |m: &Model| m.struct_quantities("brace", ids[0]).unwrap().length;
        // ends at the column faces x = 0.2 and x = 5.8
        near(len(&m), (36.0f64 + 2.4 * 2.4).sqrt() * 5.6 / 6.0, "trimmed");
        let mut cs = m.column(c2).unwrap();
        cs.at = (8.0, 0.0);
        m.update_column(c2, &cs).unwrap();
        near(
            len(&m),
            (64.0f64 + 2.4 * 2.4).sqrt() * 7.6 / 8.0,
            "follows the column",
        );
        let now = m.brace(ids[0]).unwrap();
        assert_eq!((now.start, now.end), (s.start, s.end));
        near(now.b.0, 8.0, "fallback end follows the host");
        m.undo().unwrap();
        m.undo().unwrap();
        assert!(
            m.struct_ids("brace", None).unwrap().is_empty(),
            "one undo for both"
        );
        assert!(m.check_integrity().unwrap().is_empty());
    }

    fn beam(m: &mut Model, l0: i64, t: i64, a: (f64, f64), b: (f64, f64)) {
        m.add_beam(&BeamSpec {
            level: l0,
            member_type: t,
            a: (a.0, a.1, 3.0),
            b: (b.0, b.1, 3.0),
            bulge: 0.0,
            yjust: Just::Center,
            zjust: Just::Pos,
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::Auto,
            join_end: BeamJoin::Auto,
        })
        .unwrap();
    }

    #[test]
    fn beam_system_fills_bay_and_trims_at_girders() {
        let (mut m, l0, _) = setup();
        let gt = rect(&mut m, "R 200x400", 0.2, 0.4);
        let jt = rect(&mut m, "R 100x200", 0.1, 0.2);
        let ring = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
        for i in 0..4 {
            beam(&mut m, l0, gt, ring[i], ring[(i + 1) % 4]);
        }
        let s = BeamSystemSpec {
            level: l0,
            member_type: jt,
            ring: ring.to_vec(),
            holes: vec![],
            start_edge: None,
            z: 3.0,
            angle: std::f64::consts::FRAC_PI_2,
            spacing: 1.5,
            layout: BeamLayout::FixedStart,
            end_gap: 0.01,
            zjust: Just::Pos,
        };
        let id = m.add_beam_system(&s).unwrap();
        let parts = m.struct_parts("beam_system", id).unwrap();
        assert_eq!(parts.len(), 3);
        for p in &parts {
            near(p.1, 4.0 - 2.0 * (0.1 + 0.01), "joist between girder faces");
        }
        assert_eq!(m.beam_system(id).unwrap(), s);
        let mut s2 = s.clone();
        s2.spacing = 1.0;
        m.update_beam_system(id, &s2).unwrap();
        assert_eq!(m.struct_parts("beam_system", id).unwrap().len(), 5);
        m.undo().unwrap();
        assert_eq!(m.struct_parts("beam_system", id).unwrap().len(), 3);
        m.delete_struct("beam_system", id).unwrap();
        assert!(m.beam_system(id).is_err());
        m.undo().unwrap();
        assert_eq!(m.beam_system(id).unwrap(), s);
        let mut bad = s.clone();
        bad.spacing = 0.0;
        assert!(m.add_beam_system(&bad).is_err());
    }

    #[test]
    fn brace_end_detaches_from_deleted_host_and_ignores_reused_id() {
        let (mut m, l0, l1) = setup();
        let ct = rect(&mut m, "R 400x400", 0.4, 0.4);
        let c1 = col(&mut m, l0, l1, ct, (0.0, 0.0));
        let c2 = col(&mut m, l0, l1, ct, (6.0, 0.0));
        let bt = rect(&mut m, "R 100x100", 0.1, 0.1);
        let at = |id, t| Anchor {
            host: MemberHost::Column,
            id,
            t,
        };
        let s = BraceSpec {
            level: l0,
            member_type: bt,
            a: (0.0, 0.0, 0.3),
            b: (6.0, 0.0, 2.7),
            start: Some(at(c1, 0.1)),
            end: Some(at(c2, 0.9)),
            rotation: 0.0,
        };
        let id = m.add_braces(std::slice::from_ref(&s)).unwrap()[0];
        // host moved: the fallback end follows it
        let mut cs = m.column(c2).unwrap();
        cs.at = (8.0, 1.0);
        m.update_column(c2, &cs).unwrap();
        let b = m.brace(id).unwrap();
        near(b.b.0, 8.0, "fallback x");
        near(b.b.1, 1.0, "fallback y");
        near(b.b.2, 2.7, "fallback z");
        let len = m.struct_quantities("brace", id).unwrap().length;
        // host deleted: the end is detached and stays where it was
        m.delete_column(c2).unwrap();
        let b = m.brace(id).unwrap();
        assert_eq!(b.end, None);
        assert_eq!(b.start, s.start);
        near(b.b.0, 8.0, "kept x");
        // a new column with the same id does not pick the brace up
        let c3 = col(&mut m, l0, l1, ct, (20.0, 5.0));
        assert_eq!(c3, c2, "id reused");
        assert_eq!(m.brace(id).unwrap().end, None);
        let len2 = m.struct_quantities("brace", id).unwrap().length;
        assert!(
            len2 > len,
            "free end at the old point, not cut at a face: {len2} vs {len}"
        );
        // undo: the new column, then the deletion: the brace is on the column again
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.brace(id).unwrap().end, Some(at(c2, 0.9)));
        near(
            m.struct_quantities("brace", id).unwrap().length,
            len,
            "back on the host",
        );
        assert!(m.check_integrity().unwrap().is_empty());
    }

    #[test]
    fn brace_end_is_cut_along_the_exit_face_of_a_rotated_column() {
        let (mut m, l0, l1) = setup();
        let ct = rect(&mut m, "R 400x400", 0.4, 0.4);
        let a30 = std::f64::consts::FRAC_PI_6;
        let c = m
            .add_column(&ColumnSpec {
                level: l0,
                top_level: Some(l1),
                member_type: ct,
                structural: true,
                at: (0.0, 0.0),
                base_offset: 0.0,
                top_offset: 0.0,
                rotation: a30,
                top_at: None,
            })
            .unwrap();
        let bt = rect(&mut m, "R 100x100", 0.1, 0.1);
        let id = m
            .add_braces(&[BraceSpec {
                level: l0,
                member_type: bt,
                a: (0.0, 0.0, 1.5),
                b: (6.0, 0.0, 1.5),
                start: Some(Anchor {
                    host: MemberHost::Column,
                    id: c,
                    t: 0.5,
                }),
                end: None,
                rotation: 0.0,
            }])
            .unwrap()[0];
        let mesh = m
            .struct_meshes(None)
            .unwrap()
            .into_iter()
            .find(|x| x.0 == "brace" && x.1 == id)
            .unwrap()
            .2;
        let lo = mesh.positions.iter().map(|p| p.x).fold(f64::MAX, f64::min);
        // face with a normal at 30 deg: the end is skewed, not perpendicular to the axis at the exit point
        let face = 0.2 / a30.cos();
        near(lo, face - 0.05 * a30.tan(), "oblique end along the face");
    }

    #[test]
    fn beam_system_holes_start_edge_and_fixed_count_round_trip() {
        let (mut m, l0, _) = setup();
        let jt = rect(&mut m, "R 100x200", 0.1, 0.2);
        let mut s = BeamSystemSpec {
            level: l0,
            member_type: jt,
            ring: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
            holes: vec![],
            start_edge: None,
            z: 3.0,
            angle: std::f64::consts::FRAC_PI_2,
            spacing: 1.0,
            layout: BeamLayout::FixedStart,
            end_gap: 0.0,
            zjust: Just::Pos,
        };
        let n = |m: &Model, id| m.struct_parts("beam_system", id).unwrap().len();
        let plain = m.add_beam_system(&s).unwrap();
        let k = n(&m, plain);
        // an opening across one beam splits it into two
        s.holes = vec![vec![(2.6, 1.0), (3.4, 1.0), (3.4, 3.0), (2.6, 3.0)]];
        let holed = m.add_beam_system(&s).unwrap();
        assert_eq!(n(&m, holed), k + 1);
        assert_eq!(m.beam_system(holed).unwrap(), s);
        s.holes.clear();
        s.layout = BeamLayout::FixedCount(4);
        s.start_edge = Some(1);
        let counted = m.add_beam_system(&s).unwrap();
        assert_eq!(n(&m, counted), 4);
        assert_eq!(m.beam_system(counted).unwrap(), s);
        let mut bad = s.clone();
        bad.holes = vec![vec![(5.0, 1.0), (7.0, 1.0), (7.0, 2.0), (5.0, 2.0)]];
        assert!(
            m.add_beam_system(&bad).is_err(),
            "opening outside the outline"
        );
        bad = s.clone();
        bad.start_edge = Some(9);
        assert!(m.add_beam_system(&bad).is_err());
    }

    #[test]
    fn beam_system_trims_only_at_girders_in_its_height_band() {
        let (mut m, l0, l1) = setup();
        let gt = rect(&mut m, "R 200x400", 0.2, 0.4);
        let jt = rect(&mut m, "R 100x200", 0.1, 0.2);
        let ring = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
        // contour beams on a higher level do not trim
        for i in 0..4 {
            let (a, b) = (ring[i], ring[(i + 1) % 4]);
            m.add_beam(&BeamSpec {
                level: l1,
                member_type: gt,
                a: (a.0, a.1, 3.0),
                b: (b.0, b.1, 3.0),
                bulge: 0.0,
                yjust: Just::Center,
                zjust: Just::Pos,
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: BeamJoin::Auto,
                join_end: BeamJoin::Auto,
            })
            .unwrap();
        }
        let id = m
            .add_beam_system(&BeamSystemSpec {
                level: l0,
                member_type: jt,
                ring: ring.to_vec(),
                holes: vec![],
                start_edge: None,
                z: 3.0,
                angle: std::f64::consts::FRAC_PI_2,
                spacing: 1.5,
                layout: BeamLayout::FixedStart,
                end_gap: 0.01,
                zjust: Just::Pos,
            })
            .unwrap();
        for p in m.struct_parts("beam_system", id).unwrap() {
            near(p.1, 4.0 - 2.0 * 0.01, "only the gap");
        }
    }

    #[test]
    fn takeoff_lists_new_elements_with_weight() {
        let (mut m, l0, _) = setup();
        let ct = rect(&mut m, "R 100x200", 0.1, 0.2);
        m.add_truss(&truss_spec(l0, ct, ct)).unwrap();
        let rows = m.struct_takeoff().unwrap();
        let t = rows.iter().find(|r| r.kind == "truss").expect("truss row");
        assert!(t.weight.unwrap() > 0.0 && t.volume > 0.0);
    }
}
