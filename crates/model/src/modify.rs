// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Element modification: move, copy, rotate, mirror, arrays, offset, trim/extend, split, align.
//! Each call is one transaction and one undo step; on error the state is unchanged. Openings and
//! panels follow their host via `along`/indices, dimensions that reference elements stay
//! associative, and joints and rooms are recomputed by `regenerate`.

use ddc_geom::arc::ArcAxis;
use ddc_geom::{v2, Tol};
use rusqlite::{params, types::Value, Connection};
use std::collections::HashMap;

use crate::annot::{parse_ref, refs_text};
use crate::ops::Snapshots;
use crate::walls::{parse_top_profile, split_in, top_profile_text};
use crate::{DimRef, Error, Model};

type P = (f64, f64);
const EPS: f64 = 1e-9;

/// An element that can be modified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Wall(i64),
    /// Door/window/opening: on its own it can only be moved and copied along its host.
    Opening(i64),
    Curtain(i64),
    Slab(i64),
    Room(i64),
    RoomSep(i64),
    /// Section or elevation (`view_def`).
    View(i64),
    Dim(i64),
    /// Component: with a wall host it moves along and across the wall; without a host any
    /// transform applies (a mirror toggles `mirror`).
    Component(i64),
    /// Utility run: axis points in plan, elevations are kept.
    MepRun(i64),
    /// Typed solid: any transform changes the work plane.
    Solid(i64),
    /// Imported solid without a parametric model (`mesh`): move, rotate, mirror,
    /// and copy together with its properties.
    Mesh(i64),
    /// Column, beam, utility run, ceiling: any transform in plan.
    Column(i64),
    Beam(i64),
    Ceiling(i64),
    /// Ramp: axis in plan; point order (bottom to top) is kept, also under a mirror.
    Ramp(i64),
    /// Roof: outline, slope arrow or axis in plan; elevations are kept.
    Roof(i64),
    /// Shaft: outline in plan; levels and offsets are kept, and the outline stays counter-clockwise,
    /// also under a mirror.
    Shaft(i64),
    /// Grid line: the whole axis moves, rotates, mirrors and copies (a copy gets the next name).
    Grid(i64),
    /// Reference plane: the line moves, rotates, mirrors and copies.
    RefPlane(i64),
    /// Footing, strip footing, raft, brace, truss, beam system: plan placement; elements that
    /// follow a host refuse to move alone.
    Footing(i64),
    StripFooting(i64),
    Raft(i64),
    Brace(i64),
    Truss(i64),
    BeamSystem(i64),
    /// Conceptual form: moved and copied (hosted walls and floors follow); a turn or mirror is
    /// refused, because the form history has no rotated step.
    Mass(i64),
    /// Terrain: every plan coordinate of its points, lines, boundary, regions and pads; the
    /// elevations stay.
    Topo(i64),
    /// Zone: a group of rooms, it moves, turns and copies through its rooms.
    Zone(i64),
    /// Stair, railing: plan placement. A railing on a stair rides along with it.
    Stair(i64),
    Railing(i64),
    /// Annotation (`anno`): its points in view coordinates move, turn, mirror and copy; text
    /// stays readable after a mirror.
    Anno(i64),
}

/// Spacing of a linear array.
#[derive(Clone, Copy, Debug)]
pub enum Spacing {
    /// Vector between adjacent instances.
    Step(P),
    /// Vector from the first to the last instance.
    Fit(P),
}

/// Boundary for trim/extend.
#[derive(Clone, Copy, Debug)]
pub enum Reference {
    /// Axis of a straight wall.
    Wall(i64),
    Line(P, P),
}

/// What is aligned to the reference line; the whole owning element moves.
#[derive(Clone, Copy, Debug)]
pub enum AlignEdge {
    WallAxis(i64),
    /// `face` 0 is the `offset0` face (left of the axis), 1 is the opposite one.
    WallFace {
        wall: i64,
        face: u8,
    },
    /// Edge of a slab's outer outline from vertex `idx` to the next.
    SlabEdge {
        slab: i64,
        idx: usize,
    },
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ModifyError {
    #[error("nothing selected")]
    Empty,
    #[error("{0:?} not found")]
    NotFound(Target),
    #[error("{0} is not supported for {1:?}")]
    Unsupported(&'static str, Target),
    #[error("lines are parallel")]
    Parallel,
    #[error("edge is not parallel to the reference")]
    NotParallel,
    #[error("count must be at least 2")]
    BadCount,
    #[error("more than {0} copies")]
    TooMany(usize),
    #[error("degenerate result for {0:?}")]
    Degenerate(Target),
    #[error("opening {0} would fall outside its wall")]
    OpeningOutside(i64),
    #[error("component {0} would fall outside its wall")]
    ComponentOutside(i64),
    #[error("point is not on wall {0}")]
    OffWall(i64),
    #[error("the picked point lies on the boundary")]
    OnBoundary,
}

fn me<T>(e: ModifyError) -> Result<T, Error> {
    Err(Error::Modify(e))
}

/// Orthogonal plane transform: p' = m·p + t (det −1 is a mirror).
#[derive(Clone, Copy, Debug)]
struct Xf {
    m: [[f64; 2]; 2],
    t: P,
    /// Uniform scale factor of `m` (1 for a rigid motion); directions divide it out.
    k: f64,
}

impl Xf {
    fn translate(v: P) -> Self {
        Xf {
            m: [[1.0, 0.0], [0.0, 1.0]],
            t: v,
            k: 1.0,
        }
    }
    fn rotate(c: P, ang: f64) -> Self {
        let (s, co) = ang.sin_cos();
        let m = [[co, -s], [s, co]];
        let r = mul(m, c);
        Xf {
            m,
            t: (c.0 - r.0, c.1 - r.1),
            k: 1.0,
        }
    }
    /// Uniform scale by `k > 0` about `c`.
    fn scale(c: P, k: f64) -> Self {
        Xf {
            m: [[k, 0.0], [0.0, k]],
            t: (c.0 - k * c.0, c.1 - k * c.1),
            k,
        }
    }
    fn mirror(a: P, b: P) -> Option<Self> {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l = dx.hypot(dy);
        if l < EPS {
            return None;
        }
        let (ux, uy) = (dx / l, dy / l);
        let m = [
            [2.0 * ux * ux - 1.0, 2.0 * ux * uy],
            [2.0 * ux * uy, 2.0 * uy * uy - 1.0],
        ];
        let r = mul(m, a);
        Some(Xf {
            m,
            t: (a.0 - r.0, a.1 - r.1),
            k: 1.0,
        })
    }
    fn mirrored(&self) -> bool {
        self.m[0][0] * self.m[1][1] - self.m[0][1] * self.m[1][0] < 0.0
    }
    fn p(&self, p: P) -> P {
        let r = mul(self.m, p);
        (r.0 + self.t.0, r.1 + self.t.1)
    }
    fn v(&self, v: P) -> P {
        mul(self.m, v)
    }
    /// Image of a direction with its length kept (a scale only moves points).
    fn dir(&self, v: P) -> P {
        let r = mul(self.m, v);
        if self.k == 1.0 {
            r
        } else {
            (r.0 / self.k, r.1 / self.k)
        }
    }
    fn scales(&self) -> bool {
        self.k != 1.0
    }
}

fn mul(m: [[f64; 2]; 2], p: P) -> P {
    (m[0][0] * p.0 + m[0][1] * p.1, m[1][0] * p.0 + m[1][1] * p.1)
}

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}
fn add(a: P, b: P) -> P {
    (a.0 + b.0, a.1 + b.1)
}
fn scale(a: P, k: f64) -> P {
    (a.0 * k, a.1 * k)
}
fn dot(a: P, b: P) -> f64 {
    a.0 * b.0 + a.1 * b.1
}
fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}
fn len(a: P) -> f64 {
    a.0.hypot(a.1)
}
fn unit(a: P) -> P {
    scale(a, 1.0 / len(a))
}
/// Left normal.
fn left(u: P) -> P {
    (-u.1, u.0)
}

/// Intersection of lines p+s·u and q+t·w.
fn intersect(p: P, u: P, q: P, w: P) -> Option<P> {
    let d = cross(u, w);
    if d.abs() < 1e-12 * len(u) * len(w) {
        return None;
    }
    let s = cross(sub(q, p), w) / d;
    Some(add(p, scale(u, s)))
}

/// All tables that may change; new rows are undone via the rowid watermark.
const TABLES: [&str; 46] = [
    "wall",
    "opening",
    "curtain_wall",
    "curtain_panel",
    "slab",
    "slab_vertex",
    "room",
    "room_sep",
    "view_def",
    "dim",
    "prop",
    "mep_run",
    "solid",
    "solid_host",
    "mesh",
    "comp",
    "col",
    "beam",
    "ceiling",
    "ceiling_vertex",
    "ramp",
    "roof",
    "roof_vertex",
    "skylight",
    "shaft",
    "shaft_vertex",
    "grid",
    "ref_plane",
    "iso_footing",
    "strip_footing",
    "raft",
    "raft_vertex",
    "brace",
    "truss",
    "beam_system",
    "beam_system_vertex",
    "mass",
    "mass_paint",
    "mass_host",
    "toposolid",
    "topo_point",
    "topo_region",
    "topo_pad",
    "zone",
    "zone_room",
    "anno",
];

#[derive(Clone, Copy)]
struct WallRow {
    a: P,
    b: P,
    bulge: f64,
}

fn wall_row(db: &Connection, id: i64) -> Result<WallRow, Error> {
    db.query_row(
        "SELECT ax,ay,bx,by,bulge FROM wall WHERE id=?1",
        [id],
        |r| {
            Ok(WallRow {
                a: (r.get(0)?, r.get(1)?),
                b: (r.get(2)?, r.get(3)?),
                bulge: r.get(4)?,
            })
        },
    )
    .or_else(|_| me(ModifyError::NotFound(Target::Wall(id))))
}

fn arc_of(w: &WallRow, t: Target) -> Result<ArcAxis, Error> {
    ArcAxis::new(v2(w.a.0, w.a.1), v2(w.b.0, w.b.1), w.bulge, Tol::default())
        .or_else(|_| me(ModifyError::Degenerate(t)))
}

fn axis_len(w: &WallRow, t: Target) -> Result<f64, Error> {
    if w.bulge == 0.0 {
        Ok(len(sub(w.b, w.a)))
    } else {
        Ok(arc_of(w, t)?.length())
    }
}

fn exists(db: &Connection, table: &str, id: i64) -> Result<bool, Error> {
    Ok(db.query_row(
        &format!("SELECT count(*) FROM {table} WHERE id=?1"),
        [id],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

pub(crate) fn table_of(t: Target) -> (&'static str, i64) {
    match t {
        Target::Wall(i) => ("wall", i),
        Target::Opening(i) => ("opening", i),
        Target::Curtain(i) => ("curtain_wall", i),
        Target::Slab(i) => ("slab", i),
        Target::Room(i) => ("room", i),
        Target::RoomSep(i) => ("room_sep", i),
        Target::View(i) => ("view_def", i),
        Target::Dim(i) => ("dim", i),
        Target::Component(i) => ("comp", i),
        Target::MepRun(i) => ("mep_run", i),
        Target::Solid(i) => ("solid", i),
        Target::Mesh(i) => ("mesh", i),
        Target::Column(i) => ("col", i),
        Target::Beam(i) => ("beam", i),
        Target::Ceiling(i) => ("ceiling", i),
        Target::Ramp(i) => ("ramp", i),
        Target::Roof(i) => ("roof", i),
        Target::Shaft(i) => ("shaft", i),
        Target::Grid(i) => ("grid", i),
        Target::RefPlane(i) => ("ref_plane", i),
        Target::Footing(i) => ("iso_footing", i),
        Target::StripFooting(i) => ("strip_footing", i),
        Target::Raft(i) => ("raft", i),
        Target::Brace(i) => ("brace", i),
        Target::Truss(i) => ("truss", i),
        Target::BeamSystem(i) => ("beam_system", i),
        Target::Mass(i) => ("mass", i),
        Target::Topo(i) => ("toposolid", i),
        Target::Zone(i) => ("zone", i),
        Target::Stair(i) => ("stair", i),
        Target::Railing(i) => ("railing", i),
        Target::Anno(i) => ("anno", i),
    }
}

/// Copies rows `table WHERE col=val`, overriding columns `set`; returns (old rowid, new rowid) pairs.
fn dup_rows(
    db: &Connection,
    table: &str,
    col: &str,
    val: i64,
    set: &[(&str, i64)],
) -> Result<Vec<(i64, i64)>, Error> {
    let cols: Vec<String> = {
        let mut st = db.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
        let v = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        v.into_iter().filter(|c| c != "id").collect()
    };
    let exprs: Vec<String> = cols
        .iter()
        .map(|c| match set.iter().position(|s| s.0 == c) {
            Some(k) => format!("?{}", k + 2),
            None => c.clone(),
        })
        .collect();
    let sql = format!(
        "INSERT INTO {table}({}) SELECT {} FROM {table} WHERE rowid=?1",
        cols.join(","),
        exprs.join(",")
    );
    let rows: Vec<i64> = {
        let mut st = db.prepare(&format!(
            "SELECT rowid FROM {table} WHERE {col}=?1 ORDER BY rowid"
        ))?;
        let v = st
            .query_map([val], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        v
    };
    let mut out = vec![];
    for r in rows {
        let mut args = vec![Value::Integer(r)];
        args.extend(set.iter().map(|s| Value::Integer(s.1)));
        db.execute(&sql, rusqlite::params_from_iter(args.iter()))?;
        out.push((r, db.last_insert_rowid()));
    }
    Ok(out)
}

/// Snapshots all rows of an element and its children before the change.
fn save(db: &Connection, s: &mut Snapshots, t: Target) -> Result<(), Error> {
    let (table, id) = table_of(t);
    s.save(db, table, "id", id)?;
    match t {
        Target::Wall(i) => {
            s.save(db, "opening", "wall", i)?;
            s.save(db, "comp", "host_wall", i)?;
        }
        Target::Curtain(i) => s.save(db, "curtain_panel", "cw", i)?,
        Target::Slab(i) => s.save(db, "slab_vertex", "slab", i)?,
        Target::Solid(i) => s.save(db, "solid_host", "solid", i)?,
        Target::Ceiling(i) => s.save(db, "ceiling_vertex", "ceiling", i)?,
        Target::Raft(i) => s.save(db, "raft_vertex", "raft", i)?,
        Target::BeamSystem(i) => s.save(db, "beam_system_vertex", "sys", i)?,
        Target::Railing(i) => s.save(db, "railing_vertex", "railing", i)?,
        Target::Roof(i) => {
            s.save(db, "roof_vertex", "roof", i)?;
            s.save(db, "skylight", "roof", i)?;
            s.save(db, "wall", "top_roof", i)?;
        }
        Target::Shaft(i) => s.save(db, "shaft_vertex", "shaft", i)?,
        Target::Mass(i) => {
            s.save(db, "mass_paint", "mass", i)?;
            s.save(db, "mass_host", "mass", i)?;
        }
        Target::Topo(i) => {
            s.save(db, "topo_point", "topo", i)?;
            s.save(db, "topo_region", "topo", i)?;
            s.save(db, "topo_pad", "topo", i)?;
        }
        Target::Zone(i) => {
            s.save(db, "zone_room", "zone", i)?;
            for r in crate::zones::rooms_of(db, i)? {
                s.save(db, "room", "id", r)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Mapping of old ids to new ones when copying (to rebind dimensions).
#[derive(Default)]
struct Remap {
    walls: HashMap<i64, i64>,
    openings: HashMap<i64, i64>,
    cols: HashMap<i64, i64>,
    beams: HashMap<i64, i64>,
    stairs: HashMap<i64, i64>,
}

fn validate(db: &Connection, ts: &[Target]) -> Result<(), Error> {
    if ts.is_empty() {
        return me(ModifyError::Empty);
    }
    for &t in ts {
        let (table, id) = table_of(t);
        if !exists(db, table, id)? {
            return me(ModifyError::NotFound(t));
        }
    }
    Ok(())
}

/// A group instance is held by its definition: scaling one member would break the definition.
fn reject_group_scale(db: &Connection, ts: &[Target], copy: bool) -> Result<(), Error> {
    if copy {
        return Ok(());
    }
    let mut st = db.prepare_cached("SELECT count(*) FROM group_member WHERE kind=?1 AND ref=?2")?;
    for t in ts {
        let (k, id) = table_of(*t);
        let n: i64 = st.query_row(params![k, id], |r| r.get(0))?;
        if n > 0 {
            return Err(Error::Invalid("group members cannot be scaled: ungroup first".into()));
        }
    }
    Ok(())
}

/// Kinds with a plan scale; hosted ones only together with their host.
fn scalable(db: &Connection, t: Target, ts: &[Target]) -> Result<(), Error> {
    let no = |what| me(ModifyError::Unsupported(what, t));
    match t {
        Target::Wall(_)
        | Target::Curtain(_)
        | Target::Slab(_)
        | Target::Room(_)
        | Target::RoomSep(_)
        | Target::View(_)
        | Target::Dim(_)
        | Target::MepRun(_)
        | Target::Column(_)
        | Target::Beam(_)
        | Target::Ceiling(_)
        | Target::Roof(_)
        | Target::Shaft(_)
        | Target::Grid(_)
        | Target::RefPlane(_)
        | Target::Zone(_) => Ok(()),
        Target::Opening(o) => {
            if ts.contains(&Target::Wall(host_of(db, o)?)) {
                Ok(())
            } else {
                no("scale of an opening without its wall")
            }
        }
        Target::Component(c) => {
            let h: Option<i64> =
                db.query_row("SELECT host_wall FROM comp WHERE id=?1", [c], |r| r.get(0))?;
            match h {
                Some(w) if !ts.contains(&Target::Wall(w)) => {
                    no("scale of a wall-hosted component without its wall")
                }
                _ => Ok(()),
            }
        }
        Target::Mesh(_) | Target::Solid(_) => no("scale of a body"),
        Target::Mass(_) => no("scale of a mass"),
        Target::Topo(_) => no("scale of a terrain"),
        Target::Ramp(_) => no("scale of a ramp"),
        Target::Footing(_)
        | Target::StripFooting(_)
        | Target::Raft(_)
        | Target::Brace(_)
        | Target::Truss(_)
        | Target::BeamSystem(_) => no("scale of a structural element"),
        // stairs and railings keep riser and post sizes; newer kinds are refused until handled
        _ => no("scale of this kind"),
    }
}

fn host_of(db: &Connection, opening: i64) -> Result<i64, Error> {
    Ok(
        db.query_row("SELECT wall FROM opening WHERE id=?1", [opening], |r| {
            r.get(0)
        })?,
    )
}

/// Processing order: hosts before dimensions, so dimensions see the new ids.
fn ordered(ts: &[Target]) -> Vec<Target> {
    let rank = |t: &Target| match t {
        Target::Dim(_) => 3,
        // solids after walls: a copy of a void refers to the copy of its base wall
        Target::Opening(_) | Target::Component(_) | Target::Solid(_) => 1,
        // after their hosts (column, wall, beam) so a copy can be re-hosted
        Target::Footing(_) | Target::StripFooting(_) | Target::Brace(_) => 1,
        // a railing on a stair after the stair
        Target::Railing(_) => 1,
        // walls attached to the roof are placed first: their top profile follows the final roof
        Target::Roof(_) => 2,
        // hosted walls and floors are placed first; the form then puts them on its faces
        Target::Mass(_) => 2,
        _ => 0,
    };
    let mut v: Vec<Target> = vec![];
    for t in ts {
        if !v.contains(t) {
            v.push(*t);
        }
    }
    v.sort_by_key(rank);
    v
}

/// Applies `xf` to the elements (or to their copies when `copy`); new elements follow the order of `ts`.
fn apply(
    db: &Connection,
    s: &mut Snapshots,
    ts: &[Target],
    xf: Xf,
    copy: bool,
) -> Result<Vec<Target>, Error> {
    let ts = ordered(ts);
    let mut shifted: Vec<i64> = vec![];
    let walls: Vec<i64> = ts
        .iter()
        .filter_map(|t| match t {
            Target::Wall(i) => Some(*i),
            _ => None,
        })
        .collect();
    let splines = crate::wall_spline::whole_groups(db, &walls)?;
    if copy && !splines.is_empty() {
        crate::wall_edit::ensure(db)?;
        for t in ["wall_spline", "wall_spline_piece", "wall_join"] {
            s.watermark(db, t)?;
        }
    }
    // a room of a selected zone moves with the zone, once
    let mut zoned: Vec<i64> = vec![];
    for t in &ts {
        if let Target::Zone(z) = t {
            zoned.extend(crate::zones::rooms_of(db, *z)?);
        }
    }
    let mut map = Remap::default();
    let mut out = vec![];
    if copy {
        crate::datum_xf::watermarks(db, s, &ts)?;
    }
    let fol = if copy {
        vec![]
    } else {
        crate::datum_xf::followers(db, &ts)?
    };
    for &t in &ts {
        let (table, id0) = table_of(t);
        if matches!(t, Target::Room(r) if zoned.contains(&r)) {
            continue;
        }
        if let Target::Opening(o) = t {
            if walls.contains(&host_of(db, o)?) {
                continue; // goes along with its host
            }
        }
        if let Target::Component(c) = t {
            let h: Option<i64> =
                db.query_row("SELECT host_wall FROM comp WHERE id=?1", [c], |r| r.get(0))?;
            if h.is_some_and(|h| walls.contains(&h)) {
                continue;
            }
        }
        let hosted =
            crate::struct_xf::rides_along(db, t, &ts)? || crate::datum_xf::rides_along(db, t, &ts)?;
        if hosted && !copy {
            continue; // follows its host, which moves in the same gesture
        }
        let id = if copy {
            let n = match t {
                // the name is unique: the copy takes the next free one
                Target::Grid(_) => crate::datum::copy_grid_row(db, id0)?,
                _ => dup_rows(db, table, "id", id0, &[])?[0].1,
            };
            match t {
                Target::Stair(_) => {
                    map.stairs.insert(id0, n);
                }
                Target::Railing(_) => {
                    dup_rows(db, "railing_vertex", "railing", id0, &[("railing", n)])?;
                }
                Target::Wall(_) => {
                    map.walls.insert(id0, n);
                    for (o, no) in dup_rows(db, "opening", "wall", id0, &[("wall", n)])? {
                        map.openings.insert(o, no);
                    }
                    dup_rows(db, "comp", "host_wall", id0, &[("host_wall", n)])?;
                }
                Target::Opening(_) => {
                    map.openings.insert(id0, n);
                }
                Target::Column(_) => {
                    map.cols.insert(id0, n);
                }
                Target::Beam(_) => {
                    map.beams.insert(id0, n);
                }
                Target::Curtain(_) => {
                    dup_rows(db, "curtain_panel", "cw", id0, &[("cw", n)])?;
                }
                Target::Slab(_) => {
                    dup_rows(db, "slab_vertex", "slab", id0, &[("slab", n)])?;
                }
                Target::Solid(_) => {
                    dup_rows(db, "solid_host", "solid", id0, &[("solid", n)])?;
                    for (w, nw) in &map.walls {
                        db.execute(
                            "UPDATE solid_host SET ref=?3 WHERE solid=?1 AND kind='wall' AND ref=?2",
                            params![n, w, nw],
                        )?;
                    }
                }
                Target::Ceiling(_) => {
                    dup_rows(db, "ceiling_vertex", "ceiling", id0, &[("ceiling", n)])?;
                }
                Target::Raft(_) => {
                    dup_rows(db, "raft_vertex", "raft", id0, &[("raft", n)])?;
                }
                Target::BeamSystem(_) => {
                    dup_rows(db, "beam_system_vertex", "sys", id0, &[("sys", n)])?;
                }
                Target::Roof(_) => {
                    dup_rows(db, "roof_vertex", "roof", id0, &[("roof", n)])?;
                    dup_rows(db, "skylight", "roof", id0, &[("roof", n)])?;
                }
                Target::Shaft(_) => {
                    dup_rows(db, "shaft_vertex", "shaft", id0, &[("shaft", n)])?;
                }
                Target::Mass(_) => {
                    dup_rows(db, "mass_paint", "mass", id0, &[("mass", n)])?;
                    // hosted walls copied in this batch belong to the copied mass
                    for (w, nw) in &map.walls {
                        db.execute(
                            "INSERT OR REPLACE INTO mass_host(kind,ref,mass,face,role,level,sig,orphan)                              SELECT kind,?3,?1,face,role,level,sig,orphan FROM mass_host                              WHERE kind='wall' AND ref=?2 AND mass=?4",
                            params![n, w, nw, id0],
                        )?;
                    }
                }
                Target::Topo(_) => {
                    for t in ["topo_point", "topo_region", "topo_pad"] {
                        dup_rows(db, t, "topo", id0, &[("topo", n)])?;
                    }
                }
                Target::Zone(_) => {
                    // a room is in one zone only: the copy gets copies of the rooms
                    for r in crate::zones::rooms_of(db, id0)? {
                        let nr = dup_rows(db, "room", "id", r, &[])?[0].1;
                        db.execute(
                            "INSERT INTO zone_room(room,zone) VALUES (?1,?2)",
                            params![nr, n],
                        )?;
                    }
                }
                _ => {}
            }
            if matches!(
                t,
                Target::Mesh(_)
                    | Target::Column(_)
                    | Target::Beam(_)
                    | Target::MepRun(_)
                    | Target::Ceiling(_)
            ) {
                // imported properties go along with the copy
                db.execute(
                    "INSERT INTO prop(kind,ref,key,value,unit) \n                     SELECT kind,?2,key,value,unit FROM prop WHERE kind=?3 AND ref=?1",
                    params![id0, n, table],
                )?;
            }
            if hosted {
                // the copy is placed free, then re-hosted on the copied host
                crate::struct_xf::detach(db, t, n)?;
                crate::datum_xf::detach(db, t, n)?;
            }
            n
        } else if let Target::Mesh(m) = t {
            // the body is not snapshot: undo re-applies the inverse transform
            let (sql, args) = crate::meshes::xf_inverse(m, xf.m, xf.t, 0.0)?;
            s.push_front(&sql, args);
            id0
        } else {
            save(db, s, t)?;
            id0
        };
        let nt = match t {
            Target::Wall(_) => Target::Wall(id),
            Target::Opening(_) => Target::Opening(id),
            Target::Curtain(_) => Target::Curtain(id),
            Target::Slab(_) => Target::Slab(id),
            Target::Room(_) => Target::Room(id),
            Target::RoomSep(_) => Target::RoomSep(id),
            Target::View(_) => Target::View(id),
            Target::Dim(_) => Target::Dim(id),
            Target::Component(_) => Target::Component(id),
            Target::MepRun(_) => Target::MepRun(id),
            Target::Solid(_) => Target::Solid(id),
            Target::Mesh(_) => Target::Mesh(id),
            Target::Column(_) => Target::Column(id),
            Target::Beam(_) => Target::Beam(id),
            Target::Ceiling(_) => Target::Ceiling(id),
            Target::Ramp(_) => Target::Ramp(id),
            Target::Roof(_) => Target::Roof(id),
            Target::Shaft(_) => Target::Shaft(id),
            Target::Grid(_) => Target::Grid(id),
            Target::RefPlane(_) => Target::RefPlane(id),
            Target::Footing(_) => Target::Footing(id),
            Target::StripFooting(_) => Target::StripFooting(id),
            Target::Raft(_) => Target::Raft(id),
            Target::Brace(_) => Target::Brace(id),
            Target::Truss(_) => Target::Truss(id),
            Target::BeamSystem(_) => Target::BeamSystem(id),
            Target::Mass(_) => Target::Mass(id),
            Target::Topo(_) => Target::Topo(id),
            Target::Zone(_) => Target::Zone(id),
            Target::Stair(_) => Target::Stair(id),
            Target::Railing(_) => Target::Railing(id),
            Target::Anno(_) => Target::Anno(id),
        };
        if let Target::Mass(m) = nt {
            // the form history knows a shift only; hosted elements follow it in this batch
            if xf.m != [[1.0, 0.0], [0.0, 1.0]] {
                return me(ModifyError::Unsupported("turning or mirroring a mass", nt));
            }
            s.push(crate::mass::shift_in(db, m, (xf.t.0, xf.t.1, 0.0))?);
            shifted.push(m);
        } else {
            transform(db, nt, xf, &map)?;
        }
        if let (true, Target::Room(r)) = (copy, nt) {
            crate::rooms::renumber_copy(db, r)?;
        }
        if copy && hosted {
            crate::struct_xf::rehost(db, t, nt, &map.walls, &map.cols, &map.beams)?;
            crate::datum_xf::rehost(db, t, nt, &map.stairs, xf.mirrored())?;
        }
        if copy {
            out.push(nt);
        }
    }
    if !shifted.is_empty() {
        s.push(crate::mass::follow_shifted(db, &shifted)?);
    }
    // spline walls: the exact axis follows its pieces and the chain is refitted on it
    for (sid, ps) in &splines {
        let rows: Vec<i64> = if copy {
            ps.iter()
                .map(|w| map.walls.get(w).copied().ok_or(Error::Modify(ModifyError::NotFound(Target::Wall(*w)))))
                .collect::<Result<_, _>>()?
        } else {
            ps.clone()
        };
        let (_, now, inv) = crate::wall_spline::follow_xf(db, *sid, &rows, xf.m, xf.t, copy)?;
        if copy {
            out.retain(|t| !matches!(t, Target::Wall(w) if rows.contains(w)));
            out.extend(now.into_iter().map(Target::Wall));
        } else {
            s.push_first(inv);
        }
    }
    crate::datum_xf::settle(db, s, &ts, &fol, xf.mirrored(), copy)?;
    // wall tops under roofs: only the walls this edit can reach, not the whole model
    let moved: &[Target] = if copy { &out } else { &ts };
    let pick = |f: fn(&Target) -> Option<i64>| moved.iter().filter_map(f).collect::<Vec<i64>>();
    let (ws, rs) = (pick(|t| if let Target::Wall(w) = t { Some(*w) } else { None }), pick(|t| if let Target::Roof(r) = t { Some(*r) } else { None }));
    crate::roof_auto::retrim_after_xf(db, s, &ws, &rs)?;
    Ok(out)
}

/// Mirror only re-places a member: a measured (poly) section must be symmetric about the axis
/// the mirror flips (beam: lateral `u`, column: local `v`), or an L angle keeps its flange on
/// the wrong side. `table` is "col" or "beam".
fn mirrorable(db: &Connection, table: &str, id: i64, flip_u: bool) -> Result<(), Error> {
    let shape: String = db.query_row(
        &format!("SELECT t.shape FROM {table} x JOIN member_type t ON t.id=x.mtype WHERE x.id=?1"),
        [id],
        |r| r.get(0),
    )?;
    let Some(ddc_geom::member::Shape::Poly(p)) = ddc_geom::member::Shape::parse(&shape) else {
        return Ok(());
    };
    let f = |q: (f64, f64)| if flip_u { (-q.0, q.1) } else { (q.0, -q.1) };
    let sym = p.iter().all(|&q| {
        let m = f(q);
        p.iter()
            .any(|r| (r.0 - m.0).abs() < 1e-6 && (r.1 - m.1).abs() < 1e-6)
    });
    if sym {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "{table} {id}: mirror of an asymmetric measured section is not supported"
        )))
    }
}

fn transform(db: &Connection, t: Target, xf: Xf, map: &Remap) -> Result<(), Error> {
    let mir = xf.mirrored();
    match t {
        Target::Wall(id) => {
            let w = wall_row(db, id)?;
            let (a, b) = (xf.p(w.a), xf.p(w.b));
            if xf.scales() {
                return scale_wall(db, id, &w, (a, b), xf.k);
            }
            if !mir {
                db.execute(
                    "UPDATE wall SET ax=?2,ay=?3,bx=?4,by=?5 WHERE id=?1",
                    params![id, a.0, a.1, b.0, b.1],
                )?;
                return Ok(());
            }
            // A mirror swaps the ends: layers stay on the same side of the direction,
            // and the arc sign is kept (reflection and direction reversal give two minuses).
            let l = axis_len(&w, t)?;
            let top: Option<String> =
                db.query_row("SELECT top_profile FROM wall WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
            let top = top.as_deref().and_then(parse_top_profile).map(|p| {
                let mut q: Vec<P> = p.iter().map(|x| (l - x.0, x.1)).collect();
                q.reverse();
                top_profile_text(&q)
            });
            db.execute(
                "UPDATE wall SET ax=?2,ay=?3,bx=?4,by=?5,top_profile=?6 WHERE id=?1",
                params![id, b.0, b.1, a.0, a.1, top],
            )?;
            db.execute(
                "UPDATE opening SET along=?2-along-width, swing_hinge=1-swing_hinge WHERE wall=?1",
                params![id, l],
            )?;
            // the wall's local frame is reflected: x → l − x, R(r)·Sᵐ → R(−r)·Sᵐ⁺¹
            db.execute(
                "UPDATE comp SET x=?2-x, rotation=-rotation, mirror=1-mirror WHERE host_wall=?1",
                params![id, l],
            )?;
        }
        Target::Opening(id) => {
            if xf.m != [[1.0, 0.0], [0.0, 1.0]] {
                return me(ModifyError::Unsupported("rotate/mirror", t));
            }
            let (wall, along, width): (i64, f64, f64) = db.query_row(
                "SELECT wall,along,width FROM opening WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let w = wall_row(db, wall)?;
            if w.bulge != 0.0 {
                return me(ModifyError::Unsupported("move along arc", t));
            }
            let d = sub(w.b, w.a);
            let l = len(d);
            let na = along + dot(xf.t, unit(d));
            if na < -EPS || na + width > l + EPS {
                return me(ModifyError::OpeningOutside(id));
            }
            db.execute("UPDATE opening SET along=?2 WHERE id=?1", params![id, na])?;
        }
        Target::Curtain(id) => {
            let (a, b): (P, P) = db.query_row(
                "SELECT ax,ay,bx,by FROM curtain_wall WHERE id=?1",
                [id],
                |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?))),
            )?;
            let (a, b) = (xf.p(a), xf.p(b));
            // the mesh direction u is kept, so the arc and the opening side change sign
            let k = if mir { -1.0 } else { 1.0 };
            db.execute(
                "UPDATE curtain_wall SET ax=?2,ay=?3,bx=?4,by=?5,bulge=bulge*?6 WHERE id=?1",
                params![id, a.0, a.1, b.0, b.1, k],
            )?;
            if mir {
                db.execute(
                    "UPDATE curtain_panel SET swing_side=1-swing_side WHERE cw=?1",
                    [id],
                )?;
            }
        }
        Target::Slab(id) => {
            let mut rings = slab_rings(db, id)?;
            for r in &mut rings {
                for p in &mut r.1 {
                    *p = xf.p(*p);
                }
            }
            if mir {
                for r in &mut rings {
                    r.1.reverse(); // contour orientation is kept
                }
            }
            write_rings(db, id, &rings)?;
            let slope: Option<String> =
                db.query_row("SELECT slope FROM slab WHERE id=?1", [id], |r| r.get(0))?;
            if let Some(sl) = slope {
                let mut p = crate::parse_slope(&sl)?;
                let p0 = xf.p((p.p0.x, p.p0.y));
                let g = xf.dir((p.gx, p.gy));
                p.p0 = v2(p0.0, p0.1);
                p.gx = g.0;
                p.gy = g.1;
                db.execute(
                    "UPDATE slab SET slope=?2 WHERE id=?1",
                    params![id, crate::slope_text(&p)],
                )?;
            }
        }
        Target::Room(id) => {
            let p: P = db.query_row("SELECT px,py FROM room WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
            let p = xf.p(p);
            let ol: Option<String> =
                db.query_row("SELECT outline FROM room WHERE id=?1", [id], |r| r.get(0))?;
            let ol = ol
                .and_then(|s| serde_json::from_str::<Vec<P>>(&s).ok())
                .map(|v| {
                    let mut v: Vec<P> = v.into_iter().map(|q| xf.p(q)).collect();
                    if mir {
                        v.reverse();
                    }
                    serde_json::to_string(&v)
                })
                .transpose()?;
            db.execute(
                "UPDATE room SET px=?2,py=?3,outline=?4 WHERE id=?1",
                params![id, p.0, p.1, ol],
            )?;
        }
        Target::Ramp(id) => {
            let text: String =
                db.query_row("SELECT path FROM ramp WHERE id=?1", [id], |r| r.get(0))?;
            let path: Vec<P> = crate::ramps::parse_path(&text)
                .into_iter()
                .map(|q| xf.p(q))
                .collect();
            db.execute(
                "UPDATE ramp SET path=?2 WHERE id=?1",
                params![id, crate::ramps::path_text(&path)],
            )?;
        }
        Target::Footing(_)
        | Target::StripFooting(_)
        | Target::Raft(_)
        | Target::Brace(_)
        | Target::Truss(_)
        | Target::BeamSystem(_) => {
            crate::struct_xf::transform(db, t, &|q| xf.p(q), &|q| xf.v(q), mir)?;
        }
        Target::Stair(_) | Target::Railing(_) | Target::Grid(_) | Target::RefPlane(_) => {
            crate::datum_xf::transform(db, t, &|q| xf.p(q), &|q| xf.v(q), mir)?;
        }
        Target::Roof(id) => {
            let s = crate::roofs::load(db, id)?;
            let arcs: i64 = db.query_row(
                "SELECT count(*) FROM roof_ext WHERE roof=?1 AND bulges<>'[]'",
                [id],
                |r| r.get(0),
            )?;
            if mir && arcs > 0 {
                return me(ModifyError::Unsupported("mirror of a roof with arc edges", t));
            }
            let mut new = crate::roof_xf::transformed(&s, &|q| xf.p(q), mir);
            if let crate::roofs::RoofKind::Extrusion { profile, depth, .. } = &mut new.kind {
                if xf.scales() {
                    // `u` runs along a -> b and the depth is a plan length; heights stay
                    profile.iter_mut().for_each(|p| p.0 *= xf.k);
                    *depth *= xf.k;
                }
            }
            crate::roofs::write(db, &new, Some(id))?;
            let sky: Vec<(i64, f64, f64)> = {
                let mut st = db.prepare("SELECT id,x,y FROM skylight WHERE roof=?1")?;
                let v = st
                    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                v
            };
            for (k, x, y) in sky {
                let q = xf.p((x, y));
                db.execute("UPDATE skylight SET x=?2,y=?3 WHERE id=?1", params![k, q.0, q.1])?;
            }
            // walls hanging under the roof keep a valid profile; the batch snapshot restores them
            crate::roofs::reattach_soft(db, id, crate::ops::Inv::default())?;
        }
        Target::Mass(_) => {} // shifted in `apply`, which owns the undo snapshots
        Target::Topo(id) => {
            crate::topo::transform_in(db, id, &|q| xf.p(q), mir)?;
        }
        Target::Shaft(id) => crate::shafts::transform(db, id, &|q| xf.p(q))?,
        Target::Zone(id) => {
            for r in crate::zones::rooms_of(db, id)? {
                transform(db, Target::Room(r), xf, map)?;
            }
        }
        Target::RoomSep(id) | Target::View(id) => {
            let table = table_of(t).0;
            let (a, b): (P, P) = db.query_row(
                &format!("SELECT ax,ay,bx,by FROM {table} WHERE id=?1"),
                [id],
                |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?))),
            )?;
            let (mut a, mut b) = (xf.p(a), xf.p(b));
            if mir && matches!(t, Target::View(_)) {
                std::mem::swap(&mut a, &mut b); // the view direction stays to the left of the line
            }
            db.execute(
                &format!("UPDATE {table} SET ax=?2,ay=?3,bx=?4,by=?5 WHERE id=?1"),
                params![id, a.0, a.1, b.0, b.1],
            )?;
        }
        Target::Component(id) => {
            let (x, y, rot, m, host): (f64, f64, f64, i64, Option<i64>) = db.query_row(
                "SELECT x,y,rotation,mirror,host_wall FROM comp WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            if let Some(w) = host {
                if xf.m != [[1.0, 0.0], [0.0, 1.0]] {
                    return me(ModifyError::Unsupported(
                        "rotate/mirror of a wall-hosted component",
                        t,
                    ));
                }
                let wr = wall_row(db, w)?;
                if wr.bulge != 0.0 {
                    return me(ModifyError::Unsupported("move along arc", t));
                }
                let u = unit(sub(wr.b, wr.a));
                let nx = x + dot(xf.t, u);
                if nx < -EPS || nx > len(sub(wr.b, wr.a)) + EPS {
                    return me(ModifyError::OffWall(w));
                }
                let ny = y + dot(xf.t, left(u));
                db.execute(
                    "UPDATE comp SET x=?2, y=?3 WHERE id=?1",
                    params![id, nx, ny],
                )?;
                return Ok(());
            }
            let p = xf.p((x, y));
            // L = M·R(rot)·Sᵐ; new reflection m' = m ⊕ det<0, R(rot') = L·Sᵐ'
            let k = if m != 0 { -1.0 } else { 1.0 };
            let col0 = xf.v((k * rot.cos(), k * rot.sin()));
            let nm = (m != 0) != mir;
            let k2 = if nm { -1.0 } else { 1.0 };
            let nr = (k2 * col0.1).atan2(k2 * col0.0);
            // a plan reflection S passes through the plan turn as S·T·S on the tilt
            let tilt: Option<String> = db.query_row("SELECT tilt FROM comp WHERE id=?1", [id], |r| r.get(0))?;
            let tilt = crate::components::tilt_parse(tilt).map(|q| match mir {
                true => [q[0], -q[1], -q[2], q[3]],
                false => q,
            });
            db.execute(
                "UPDATE comp SET x=?2, y=?3, rotation=?4, mirror=?5, tilt=?6 WHERE id=?1",
                params![id, p.0, p.1, nr, nm as i64, crate::components::tilt_text(tilt)],
            )?;
        }
        Target::Mesh(id) => crate::meshes::xf_mesh(db, id, xf.m, xf.t, 0.0)?,
        Target::Column(id) => {
            if mir {
                mirrorable(db, "col", id, false)?;
            }
            let (x, y, rot, tx, ty): (f64, f64, f64, Option<f64>, Option<f64>) = db.query_row(
                "SELECT x,y,rotation,tx,ty FROM col WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            let p = xf.p((x, y));
            let top = tx.zip(ty).map(|t| xf.p(t));
            let d = xf.v((rot.cos(), rot.sin()));
            db.execute(
                "UPDATE col SET x=?2,y=?3,rotation=?4,tx=?5,ty=?6 WHERE id=?1",
                params![
                    id,
                    p.0,
                    p.1,
                    d.1.atan2(d.0),
                    top.map(|t| t.0),
                    top.map(|t| t.1)
                ],
            )?;
        }
        Target::Beam(id) => {
            let (a, b): (P, P) =
                db.query_row("SELECT ax,ay,bx,by FROM beam WHERE id=?1", [id], |r| {
                    Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?)))
                })?;
            let (a, b) = (xf.p(a), xf.p(b));
            // reflection: the arc and the section rotation change sign
            if mir {
                mirrorable(db, "beam", id, true)?;
            }
            let k = if mir { -1.0 } else { 1.0 };
            db.execute(
                "UPDATE beam SET ax=?2,ay=?3,bx=?4,by=?5,bulge=bulge*?6,rotation=rotation*?6                  WHERE id=?1",
                params![id, a.0, a.1, b.0, b.1, k],
            )?;
        }
        Target::MepRun(id) => {
            let pts: String =
                db.query_row("SELECT points FROM mep_run WHERE id=?1", [id], |r| r.get(0))?;
            let pts = crate::parse_points(&pts)
                .ok_or_else(|| Error::Invalid(format!("run {id}: bad points")))?;
            let moved: Vec<(f64, f64, f64)> = pts
                .iter()
                .map(|&(x, y, z)| {
                    let p = xf.p((x, y));
                    (p.0, p.1, z)
                })
                .collect();
            db.execute(
                "UPDATE mep_run SET points=?2 WHERE id=?1",
                params![id, crate::points_text(&moved)],
            )?;
        }
        Target::Solid(id) => crate::solids::transform(db, id, xf.m, xf.t)?,
        Target::Ceiling(id) => {
            let rings = crate::shafts::ceiling_rings(db, id)?;
            db.execute("DELETE FROM ceiling_vertex WHERE ceiling=?1", [id])?;
            for (ri, r) in rings.iter().enumerate() {
                let mut r: Vec<P> = r.iter().map(|p| xf.p(*p)).collect();
                if mir {
                    r.reverse(); // contour orientation is kept
                }
                for (i, p) in r.iter().enumerate() {
                    db.execute(
                        "INSERT INTO ceiling_vertex(ceiling,ring,idx,x,y) VALUES (?1,?2,?3,?4,?5)",
                        params![id, ri as i64, i as i64, p.0, p.1],
                    )?;
                }
            }
        }
        Target::Dim(id) => {
            let (refs, dx, dy): (String, f64, f64) =
                db.query_row("SELECT refs,dx,dy FROM dim WHERE id=?1", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
            let mut rs = parse_refs(&refs)?;
            for r in &mut rs {
                remap_ref(r, &xf, map);
            }
            let d = xf.dir((dx, dy));
            let k = if mir { -1.0 } else { 1.0 };
            db.execute(
                "UPDATE dim SET refs=?2,dx=?3,dy=?4,side=side*?5 WHERE id=?1",
                params![id, refs_text(&rs), d.0, d.1, k],
            )?;
        }
        Target::Anno(id) => crate::annot_text::transform_row(db, id, xf.m, xf.t)?,
    }
    Ok(())
}

/// Scaled wall: ends and arc radius follow `k` (the bulge is scale free), hosted openings and
/// components keep their place along the axis in proportion; thickness, heights and opening
/// sizes stay, so an opening that no longer fits refuses the whole gesture.
fn scale_wall(db: &Connection, id: i64, w: &WallRow, (a, b): (P, P), k: f64) -> Result<(), Error> {
    let l = axis_len(w, Target::Wall(id))? * k;
    let top: Option<String> =
        db.query_row("SELECT top_profile FROM wall WHERE id=?1", [id], |r| r.get(0))?;
    let top = top.as_deref().and_then(parse_top_profile).map(|p| {
        let q: Vec<P> = p.iter().map(|x| (x.0 * k, x.1)).collect();
        top_profile_text(&q)
    });
    let ops: Vec<(i64, f64, f64)> = {
        let mut st = db.prepare("SELECT id,along,width FROM opening WHERE wall=?1")?;
        let v = st
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        v
    };
    for (o, along, width) in ops {
        let na = along * k;
        if na < -EPS || na + width > l + EPS {
            return me(ModifyError::OpeningOutside(o));
        }
        db.execute("UPDATE opening SET along=?2 WHERE id=?1", params![o, na])?;
    }
    db.execute("UPDATE comp SET x=x*?2 WHERE host_wall=?1", params![id, k])?;
    if top.is_some() {
        db.execute(
            "UPDATE wall SET ax=?2,ay=?3,bx=?4,by=?5,top_profile=?6 WHERE id=?1",
            params![id, a.0, a.1, b.0, b.1, top],
        )?;
    } else {
        db.execute(
            "UPDATE wall SET ax=?2,ay=?3,bx=?4,by=?5 WHERE id=?1",
            params![id, a.0, a.1, b.0, b.1],
        )?;
    }
    Ok(())
}

fn parse_refs(s: &str) -> Result<Vec<DimRef>, Error> {
    s.split(';')
        .filter(|x| !x.is_empty())
        .map(|x| parse_ref(x).ok_or_else(|| Error::Invalid(format!("bad dim ref {x}"))))
        .collect()
}

fn remap_ref(r: &mut DimRef, xf: &Xf, map: &Remap) {
    let w = |id: &mut i64| {
        if let Some(n) = map.walls.get(id) {
            *id = *n;
        }
    };
    match r {
        DimRef::Point(p) => *p = xf.p(*p),
        DimRef::WallEnd { wall, .. }
        | DimRef::AxisEnd { wall, .. }
        | DimRef::AxisLine { wall }
        | DimRef::ArcFace { wall, .. }
        | DimRef::WallLevel { wall, .. }
        | DimRef::GridAt { wall, .. } => w(wall),
        DimRef::OpeningEdge { opening, .. } => {
            if let Some(n) = map.openings.get(opening) {
                *opening = *n;
            }
        }
    }
}

fn write_rings(db: &Connection, slab: i64, rings: &[(i64, Vec<P>)]) -> Result<(), Error> {
    // the same vertices are not rewritten: a pinned slab would fail on a non-positional edit
    let same: Vec<Vec<P>> = rings.iter().map(|r| r.1.clone()).collect();
    if crate::slabs::rings(db, slab)? == same {
        return Ok(());
    }
    db.execute("DELETE FROM slab_vertex WHERE slab=?1", [slab])?;
    for (ring, pts) in rings {
        for (i, p) in pts.iter().enumerate() {
            db.execute(
                "INSERT INTO slab_vertex(slab,ring,idx,x,y) VALUES (?1,?2,?3,?4,?5)",
                params![slab, ring, i as i64, p.0, p.1],
            )?;
        }
    }
    Ok(())
}

fn wall_thickness(db: &Connection, id: i64) -> Result<(f64, f64), Error> {
    let (total, off0): (f64, Option<f64>) = db.query_row(
        "SELECT COALESCE((SELECT SUM(thickness) FROM wall_type_layer WHERE wtype=wall.wtype),0), offset0 \
         FROM wall WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((total, off0.unwrap_or(total / 2.0)))
}

/// New end of a straight wall: moving the start changes the openings' `along` and the top profile.
fn set_end(db: &Connection, id: i64, end_b: bool, p: P) -> Result<(), Error> {
    crate::wall_spline::refuse_piece(db, id)?;
    let w = wall_row(db, id)?;
    let t = Target::Wall(id);
    let u = unit(sub(w.b, w.a));
    let (na, nb) = if end_b { (w.a, p) } else { (p, w.b) };
    let nl = dot(sub(nb, na), u);
    if nl < Tol::default().length {
        return me(ModifyError::Degenerate(t));
    }
    let shift = if end_b { 0.0 } else { dot(sub(p, w.a), u) };
    let mut st = db.prepare("SELECT id,along,width FROM opening WHERE wall=?1")?;
    let ops = st
        .query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (o, along, width) in ops {
        let na = along - shift;
        if na < -EPS || na + width > nl + EPS {
            return me(ModifyError::OpeningOutside(o));
        }
        db.execute("UPDATE opening SET along=?2 WHERE id=?1", params![o, na])?;
    }
    // components on a wall lie in its local frame: x is measured from the start
    let mut st = db.prepare("SELECT id,x FROM comp WHERE host_wall=?1")?;
    let comps = st
        .query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (c, x) in comps {
        let nx = x - shift;
        if !nx.is_finite() || nx < -EPS || nx > nl + EPS {
            return me(ModifyError::ComponentOutside(c));
        }
        db.execute("UPDATE comp SET x=?2 WHERE id=?1", params![c, nx])?;
    }
    if shift != 0.0 {
        let top: Option<String> =
            db.query_row("SELECT top_profile FROM wall WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
        if let Some(p) = top.as_deref().and_then(parse_top_profile) {
            let q: Vec<P> = p.iter().map(|x| (x.0 - shift, x.1)).collect();
            db.execute(
                "UPDATE wall SET top_profile=?2 WHERE id=?1",
                params![id, top_profile_text(&q)],
            )?;
        }
    }
    db.execute(
        "UPDATE wall SET ax=?2,ay=?3,bx=?4,by=?5 WHERE id=?1",
        params![id, na.0, na.1, nb.0, nb.1],
    )?;
    Ok(())
}

fn straight(db: &Connection, id: i64, what: &'static str) -> Result<WallRow, Error> {
    let w = wall_row(db, id)?;
    if w.bulge != 0.0 {
        return me(ModifyError::Unsupported(what, Target::Wall(id)));
    }
    Ok(w)
}

/// End of the wall nearest to point `p` (true means end b).
fn near_end(w: &WallRow, p: P) -> bool {
    len(sub(w.b, p)) < len(sub(w.a, p))
}

impl Model {
    fn modify<T>(
        &mut self,
        kind: &str,
        f: impl FnOnce(&Connection, &mut Snapshots) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.batch_in(kind, &TABLES, |db, s| {
            let out = f(db, s)?;
            crate::constraints::verify(db)?;
            Ok(out)
        })
    }

    /// Move by vector `v`.
    pub fn move_elements(&mut self, ts: &[Target], v: P) -> Result<(), Error> {
        self.mep_follow("wall_move", ts, false, |m| {
            m.modify("wall_move", |db, s| {
                validate(db, ts)?;
                apply(db, s, ts, Xf::translate(v), false).map(|_| ())
            })
        })
    }

    /// Vertical shift of solids, masses and terrains by `dz` metres: one operation, one undo
    /// step. Other kinds are refused and nothing changes.
    pub fn lift_elements(&mut self, ts: &[Target], dz: f64) -> Result<(), Error> {
        if !dz.is_finite() {
            return Err(Error::Invalid("lift: offset is not finite".into()));
        }
        self.modify("elem_lift", |db, s| {
            validate(db, ts)?;
            let mut shifted = vec![];
            for &t in &ordered(ts) {
                save(db, s, t)?;
                match t {
                    Target::Solid(id) => crate::solids::lift(db, id, dz)?,
                    Target::Mass(id) => {
                        s.push(crate::mass::shift_in(db, id, (0.0, 0.0, dz))?);
                        shifted.push(id);
                    }
                    Target::Topo(id) => crate::topo::lift_in(db, id, dz)?,
                    _ => return me(ModifyError::Unsupported("vertical move", t)),
                }
            }
            if !shifted.is_empty() {
                s.push(crate::mass::follow_shifted(db, &shifted)?);
            }
            Ok(())
        })
    }

    /// Copies with offsets `vs` (several copies in one operation).
    pub fn copy_elements(&mut self, ts: &[Target], vs: &[P]) -> Result<Vec<Target>, Error> {
        self.modify("wall_copy", |db, s| {
            validate(db, ts)?;
            if vs.is_empty() {
                return me(ModifyError::Empty);
            }
            let mut out = vec![];
            for v in vs {
                out.extend(apply(db, s, ts, Xf::translate(*v), true)?);
            }
            Ok(out)
        })
    }

    /// Rotation about `c` by `angle` radians counter-clockwise; with `copy` a copy is rotated.
    pub fn rotate_elements(
        &mut self,
        ts: &[Target],
        c: P,
        angle: f64,
        copy: bool,
    ) -> Result<Vec<Target>, Error> {
        self.mep_follow("wall_rotate", ts, copy, |m| {
            m.modify("wall_rotate", |db, s| {
                validate(db, ts)?;
                crate::groups::reject_turn(db, ts, copy)?;
                apply(db, s, ts, Xf::rotate(c, angle), copy)
            })
        })
    }

    /// Mirror about line a→b; hinges and opening sides are reflected geometrically.
    pub fn mirror_elements(
        &mut self,
        ts: &[Target],
        a: P,
        b: P,
        copy: bool,
    ) -> Result<Vec<Target>, Error> {
        self.mep_follow("wall_mirror", ts, copy, |m| {
            m.modify("wall_mirror", |db, s| {
                validate(db, ts)?;
                crate::groups::reject_turn(db, ts, copy)?;
                let xf = Xf::mirror(a, b).ok_or(Error::Modify(ModifyError::Parallel))?;
                apply(db, s, ts, xf, copy)
            })
        })
    }

    /// Uniform scale by `k` about `c` (`copy`: the scaled copies are added and returned). Plan
    /// geometry scales: wall ends, rings, grids, lines, arc radii and hosted offsets along the
    /// host; thicknesses, type sizes, heights and point-element sizes stay. Refused with a
    /// reason and nothing changed: `k <= 0`, group members (unless copied), pinned elements and
    /// kinds without a plan scale.
    pub fn scale_elements(
        &mut self,
        ts: &[Target],
        c: P,
        k: f64,
        copy: bool,
    ) -> Result<Vec<Target>, Error> {
        if !(k.is_finite() && k > 0.0) {
            return Err(Error::Invalid(format!(
                "scale factor must be greater than zero, got {k}"
            )));
        }
        if !(c.0.is_finite() && c.1.is_finite()) {
            return Err(Error::Invalid("scale: base point is not finite".into()));
        }
        self.mep_follow("elem_scale", ts, copy, |m| {
            m.modify("elem_scale", |db, s| {
                validate(db, ts)?;
                reject_group_scale(db, ts, copy)?;
                for &t in ts {
                    scalable(db, t, ts)?;
                }
                apply(db, s, ts, Xf::scale(c, k), copy)
            })
        })
    }

    /// Linear array: `count` instances including the original, no group.
    pub fn array_linear(
        &mut self,
        ts: &[Target],
        count: usize,
        spacing: Spacing,
    ) -> Result<Vec<Target>, Error> {
        self.modify("wall_array", |db, s| {
            validate(db, ts)?;
            if count < 2 {
                return me(ModifyError::BadCount);
            }
            let step = match spacing {
                Spacing::Step(v) => v,
                Spacing::Fit(v) => scale(v, 1.0 / (count - 1) as f64),
            };
            let mut out = vec![];
            for k in 1..count {
                out.extend(apply(
                    db,
                    s,
                    ts,
                    Xf::translate(scale(step, k as f64)),
                    true,
                )?);
            }
            Ok(out)
        })
    }

    /// Radial array about `c`: `count` instances over the angle `total` (2π is a full circle).
    pub fn array_radial(
        &mut self,
        ts: &[Target],
        count: usize,
        c: P,
        total: f64,
    ) -> Result<Vec<Target>, Error> {
        self.modify("wall_array_radial", |db, s| {
            validate(db, ts)?;
            if count < 2 {
                return me(ModifyError::BadCount);
            }
            let full = (total.abs() - std::f64::consts::TAU).abs() < 1e-9;
            let step = total / if full { count } else { count - 1 } as f64;
            let mut out = vec![];
            for k in 1..count {
                out.extend(apply(db, s, ts, Xf::rotate(c, step * k as f64), true)?);
            }
            Ok(out)
        })
    }

    /// Parallel copy of a wall at `dist` to the left (`left`) or right of the axis; an arc is offset concentrically.
    pub fn offset_wall(&mut self, id: i64, dist: f64, left_side: bool) -> Result<i64, Error> {
        self.modify("wall_offset", |db, _| {
            // an offset copy of one arc of the chain would not follow the spline axis
            crate::wall_spline::refuse_piece(db, id)?;
            let w = wall_row(db, id)?;
            let t = Target::Wall(id);
            if !(dist > 0.0 && dist.is_finite()) {
                return me(ModifyError::Degenerate(t));
            }
            let d = if left_side { dist } else { -dist };
            let n = dup_rows(db, "wall", "id", id, &[])?[0].1;
            let (a, b) = if w.bulge == 0.0 {
                let o = scale(left(unit(sub(w.b, w.a))), d);
                (add(w.a, o), add(w.b, o))
            } else {
                let arc = arc_of(&w, t)?;
                let c = (arc.center.x, arc.center.y);
                // the left side of a counter-clockwise arc faces the centre
                let r = arc.radius - if w.bulge > 0.0 { d } else { -d };
                if r < Tol::default().length {
                    return me(ModifyError::Degenerate(t));
                }
                let k = r / arc.radius;
                (add(c, scale(sub(w.a, c), k)), add(c, scale(sub(w.b, c), k)))
            };
            db.execute(
                "UPDATE wall SET ax=?2,ay=?3,bx=?4,by=?5 WHERE id=?1",
                params![n, a.0, a.1, b.0, b.1],
            )?;
            Ok(n)
        })
    }

    /// Offsets the edges of a slab's outer outline by `dist` outward (< 0 inward).
    pub fn offset_slab(&mut self, id: i64, dist: f64) -> Result<(), Error> {
        self.modify("slab_offset", |db, s| {
            let t = Target::Slab(id);
            validate(db, &[t])?;
            save(db, s, t)?;
            let rings = slab_rings(db, id)?;
            let mut out = vec![];
            for (ring, pts) in rings {
                if ring != 0 {
                    out.push((ring, pts));
                    continue;
                }
                let n = pts.len();
                let mut q = vec![];
                for i in 0..n {
                    let (p0, p1, p2) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
                    let (u0, u1) = (unit(sub(p1, p0)), unit(sub(p2, p1)));
                    // counter-clockwise outline: outward is the right normal
                    let (o0, o1) = (scale(left(u0), -dist), scale(left(u1), -dist));
                    let p = intersect(add(p0, o0), u0, add(p1, o1), u1).unwrap_or(add(p1, o1));
                    q.push(p);
                }
                if area(&q) <= Tol::default().length || !simple(&q) || area(&q) * area(&pts) < 0.0 {
                    return me(ModifyError::Degenerate(t));
                }
                out.push((0, q));
            }
            write_rings(db, id, &out)
        })
    }

    /// Trim/extend two straight walls to a corner: the ends nearest the intersection meet there.
    pub fn trim_corner(&mut self, w1: i64, w2: i64) -> Result<(), Error> {
        self.trim_corner_at(w1, w2, None, None)
    }

    /// Like `trim_corner`; with a click point per wall and a corner inside that wall, the
    /// clicked part is kept. A corner outside the wall extends its nearest end; a click
    /// exactly on the corner is refused.
    pub fn trim_corner_at(
        &mut self,
        w1: i64,
        w2: i64,
        click1: Option<P>,
        click2: Option<P>,
    ) -> Result<(), Error> {
        self.modify("wall_trim_corner", |db, s| {
            let (a, b) = (straight(db, w1, "trim")?, straight(db, w2, "trim")?);
            if w1 == w2 {
                return me(ModifyError::Parallel);
            }
            let p = intersect(a.a, sub(a.b, a.a), b.a, sub(b.b, b.a))
                .ok_or(Error::Modify(ModifyError::Parallel))?;
            let mut ends = vec![];
            for (id, w, click) in [(w1, a, click1), (w2, b, click2)] {
                let end_b = match click {
                    Some(c) => {
                        let (u, l) = (unit(sub(w.b, w.a)), len(sub(w.b, w.a)));
                        let (tp, tc) = (dot(sub(p, w.a), u), dot(sub(c, w.a), u));
                        let eps = Tol::default().length;
                        if tp > eps && tp < l - eps {
                            if (tc - tp).abs() < eps {
                                return me(ModifyError::OnBoundary);
                            }
                            tc < tp
                        } else {
                            near_end(&w, p)
                        }
                    }
                    None => near_end(&w, p),
                };
                ends.push((id, end_b));
            }
            for (id, end_b) in ends {
                save(db, s, Target::Wall(id))?;
                set_end(db, id, end_b, p)?;
            }
            Ok(())
        })
    }

    /// Trim/extend a straight wall to the reference line (nearest end).
    pub fn trim_to(&mut self, wall: i64, r: Reference) -> Result<(), Error> {
        self.trim_to_at(wall, r, None)
    }

    /// Like `trim_to`; with a click point and a boundary crossing the wall, the clicked part
    /// is kept and the other end moves to the boundary. A boundary outside the wall extends
    /// the nearest end; a click exactly on the boundary is refused.
    pub fn trim_to_at(&mut self, wall: i64, r: Reference, click: Option<P>) -> Result<(), Error> {
        self.modify("wall_trim", |db, s| {
            let w = straight(db, wall, "trim")?;
            let (q, d) = match r {
                Reference::Wall(o) => {
                    if o == wall {
                        return me(ModifyError::Parallel);
                    }
                    let x = straight(db, o, "trim reference")?;
                    (x.a, sub(x.b, x.a))
                }
                Reference::Line(a, b) => (a, sub(b, a)),
            };
            if len(d) < EPS {
                return me(ModifyError::Parallel);
            }
            let p =
                intersect(w.a, sub(w.b, w.a), q, d).ok_or(Error::Modify(ModifyError::Parallel))?;
            let end_b = match click {
                Some(c) => {
                    let (u, l) = (unit(sub(w.b, w.a)), len(sub(w.b, w.a)));
                    let (tp, tc) = (dot(sub(p, w.a), u), dot(sub(c, w.a), u));
                    let eps = Tol::default().length;
                    if tp > eps && tp < l - eps {
                        if (tc - tp).abs() < eps {
                            return me(ModifyError::OnBoundary);
                        }
                        tc < tp
                    } else {
                        near_end(&w, p)
                    }
                }
                None => near_end(&w, p),
            };
            save(db, s, Target::Wall(wall))?;
            set_end(db, wall, end_b, p)
        })
    }

    /// Splits a wall at point `p` (projected onto the axis): openings stay on their parts,
    /// dimensions to end b move to the new part. Returns the id of the right part.
    pub fn split_wall_at(&mut self, wall: i64, p: P) -> Result<i64, Error> {
        self.modify("wall_split_at", |db, s| {
            let w = wall_row(db, wall)?;
            let tg = Target::Wall(wall);
            let (t, l) = if w.bulge == 0.0 {
                let d = sub(w.b, w.a);
                (dot(sub(p, w.a), unit(d)), len(d))
            } else {
                let arc = arc_of(&w, tg)?;
                (arc.param_of(v2(p.0, p.1)), arc.length())
            };
            let eps = Tol::default().length;
            if !(t > eps && t < l - eps) {
                return me(ModifyError::OffWall(wall));
            }
            split_t_in(db, s, wall, t)
        })
    }

    /// Splits a wall into `n` parts of equal axis length in one journal entry (one undo).
    /// Returns the ids of all parts in order from end a; the first part keeps the wall id.
    pub fn divide_wall(&mut self, wall: i64, n: usize) -> Result<Vec<i64>, Error> {
        if !(2..=MAX_DIVIDE).contains(&n) {
            return Err(Error::Invalid(format!("divide: parts {n} outside 2..={MAX_DIVIDE}")));
        }
        self.modify("wall_divide", |db, s| {
            let w = wall_row(db, wall)?;
            let l = if w.bulge == 0.0 {
                len(sub(w.b, w.a))
            } else {
                arc_of(&w, Target::Wall(wall))?.length()
            };
            let step = l / n as f64;
            if step <= Tol::default().length {
                return me(ModifyError::OffWall(wall));
            }
            let mut out = vec![wall];
            let mut cur = wall;
            for _ in 1..n {
                cur = split_t_in(db, s, cur, step)?;
                out.push(cur);
            }
            Ok(out)
        })
    }
}

/// Upper bound for [`Model::divide_wall`]: keeps one call from flooding the model.
pub const MAX_DIVIDE: usize = 100;

/// Splits a wall at distance `t` along the axis inside an open edit: wall snapshot, split,
/// dimensions to end b move to the new part. Returns the id of the new (right) part.
fn split_t_in(db: &Connection, s: &mut Snapshots, wall: i64, t: f64) -> Result<i64, Error> {
    save(db, s, Target::Wall(wall))?;
    let dims: Vec<(i64, String)> = {
        let mut st = db.prepare("SELECT id,refs FROM dim WHERE kind<>'tag'")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        v
    };
    let (new, _) = split_in(db, wall, t)?;
    for (d, refs) in dims {
        let mut rs = parse_refs(&refs)?;
        let mut hit = false;
        for r in &mut rs {
            match r {
                DimRef::WallEnd {
                    wall: x, end: 1, ..
                }
                | DimRef::AxisEnd { wall: x, end: 1 }
                    if *x == wall =>
                {
                    *x = new;
                    hit = true;
                }
                _ => {}
            }
        }
        if hit {
            s.save(db, "dim", "id", d)?;
            db.execute(
                "UPDATE dim SET refs=?2 WHERE id=?1",
                params![d, refs_text(&rs)],
            )?;
        }
    }
    Ok(new)
}

impl Model {

    /// Align: the edge/axis/face is placed on the reference line a→b (must be parallel),
    /// and the element moves along the normal together with its hosted openings.
    pub fn align(&mut self, e: AlignEdge, a: P, b: P) -> Result<(), Error> {
        self.modify("wall_align", |db, s| {
            let (target, p, u) = match e {
                AlignEdge::WallAxis(w) | AlignEdge::WallFace { wall: w, .. } => {
                    let r = straight(db, w, "align")?;
                    let u = unit(sub(r.b, r.a));
                    let off = match e {
                        AlignEdge::WallFace { face, .. } => {
                            let (total, off0) = wall_thickness(db, w)?;
                            if face == 0 {
                                off0
                            } else {
                                off0 - total
                            }
                        }
                        _ => 0.0,
                    };
                    (Target::Wall(w), add(r.a, scale(left(u), off)), u)
                }
                AlignEdge::SlabEdge { slab, idx } => {
                    let t = Target::Slab(slab);
                    validate(db, &[t])?;
                    let rings = slab_rings(db, slab)?;
                    let outer = rings
                        .iter()
                        .find(|r| r.0 == 0)
                        .map(|r| r.1.clone())
                        .unwrap_or_default();
                    if idx >= outer.len() {
                        return me(ModifyError::NotFound(t));
                    }
                    let (p0, p1) = (outer[idx], outer[(idx + 1) % outer.len()]);
                    (t, p0, unit(sub(p1, p0)))
                }
            };
            let d = sub(b, a);
            if len(d) < EPS {
                return me(ModifyError::Parallel);
            }
            let ud = unit(d);
            if cross(u, ud).abs() > 1e-9 {
                return me(ModifyError::NotParallel);
            }
            let n = left(ud);
            let v = scale(n, dot(sub(a, p), n));
            apply(db, s, &[target], Xf::translate(v), false).map(|_| ())
        })
    }
}

fn slab_rings(db: &Connection, id: i64) -> Result<Vec<(i64, Vec<P>)>, Error> {
    let mut st = db.prepare("SELECT ring,x,y FROM slab_vertex WHERE slab=?1 ORDER BY ring, idx")?;
    let rows = st.query_map([id], |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?))))?;
    let mut rings: Vec<(i64, Vec<P>)> = vec![];
    for r in rows {
        let (ring, p) = r?;
        if rings.last().map(|x| x.0) != Some(ring) {
            rings.push((ring, vec![]));
        }
        if let Some(l) = rings.last_mut() {
            l.1.push(p);
        }
    }
    Ok(rings)
}

fn area(p: &[P]) -> f64 {
    let n = p.len();
    (0..n).map(|i| cross(p[i], p[(i + 1) % n])).sum::<f64>() / 2.0
}

/// No intersections between non-adjacent edges.
fn simple(p: &[P]) -> bool {
    let n = p.len();
    let hit = |a: P, b: P, c: P, d: P| {
        let o = |p: P, q: P, r: P| cross(sub(q, p), sub(r, p));
        o(a, b, c) * o(a, b, d) < 0.0 && o(c, d, a) * o(c, d, b) < 0.0
    };
    for i in 0..n {
        for j in i + 2..n {
            if i == 0 && j == n - 1 {
                continue;
            }
            if hit(p[i], p[(i + 1) % n], p[j], p[(j + 1) % n]) {
                return false;
            }
        }
    }
    true
}

#[path = "modify_pattern.rs"]
mod pattern;
#[path = "modify_z.rs"]
mod z;
pub use pattern::{
    path_preview, surface_preview, GridDiv, PathCount, PatternFace, PatternPlacement,
    PatternPreview, SurfaceUp, MAX_PATTERN_COPIES,
};

#[cfg(test)]
#[path = "modify_tests.rs"]
mod tests;
