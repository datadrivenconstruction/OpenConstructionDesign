//! Boundary edges linked to walls: an outline edge of a slab, roof, ceiling or shaft picked
//! from a wall face or axis keeps a link and is re-derived whenever its wall moves.
//! Table (created by name, no schema step): `boundary_link(host_kind, host, edge, wall, face,
//! offset, state)`; `edge` is the curve index in the host sketch, `state` 0 linked, 1 source
//! wall deleted (shown as a warning until accepted or re-picked), 2 cannot follow (arc wall,
//! or the followed outline would be invalid). The rebuild runs inside the operation that
//! changed the walls, so the host follows in the same undo step. Rooms are bounded by walls
//! directly and follow without links.

use ddc_geom::math::{v2, V2};
use ddc_geom::sketch::{Sketch, SketchCurve};
use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension};

use crate::changes::Touch;
use crate::ops::Inv;
use crate::sketch::{parse_sketch, sketch_text, SketchHost};
use crate::{Error, Model};

pub(crate) const DDL: &str = "CREATE TABLE IF NOT EXISTS boundary_link(host_kind TEXT NOT NULL, \
    host INTEGER NOT NULL, edge INTEGER NOT NULL, wall INTEGER NOT NULL, face TEXT NOT NULL, \
    offset REAL NOT NULL, state INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(host_kind, host, edge));";

/// Idempotent, by table name.
pub(crate) fn ensure(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

/// Distance at which a point lies on a line or two vertices coincide.
const TOL: f64 = 1e-6;

/// Hosts that carry linked edges: sketch kind, vertex table, owner column, ring column.
const HOSTS: &[HostRow] = &[
    ("slab", "slab_vertex", "slab", "ring"),
    ("roof", "roof_vertex", "roof", "0"),
    ("ceiling", "ceiling_vertex", "ceiling", "ring"),
    ("shaft", "shaft_vertex", "shaft", "0"),
];

type HostRow = (&'static str, &'static str, &'static str, &'static str);

fn host_of(kind: &str) -> Option<&'static HostRow> {
    HOSTS.iter().find(|h| h.0 == kind)
}

/// Which line of the wall the edge follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkFace {
    Axis,
    /// Face left of the wall direction start -> end.
    Left,
    Right,
}

impl LinkFace {
    pub fn key(self) -> &'static str {
        match self {
            LinkFace::Axis => "axis",
            LinkFace::Left => "left",
            LinkFace::Right => "right",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "axis" => Some(LinkFace::Axis),
            "left" => Some(LinkFace::Left),
            "right" => Some(LinkFace::Right),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    Linked,
    /// The source wall was deleted: the edge keeps its last place and is shown as a warning.
    SourceLost,
    /// The edge cannot follow (arc wall, no thickness for a face, invalid outline).
    Blocked,
}

impl LinkState {
    fn code(self) -> i64 {
        match self {
            LinkState::Linked => 0,
            LinkState::SourceLost => 1,
            LinkState::Blocked => 2,
        }
    }
    fn from_code(c: i64) -> Self {
        match c {
            1 => LinkState::SourceLost,
            2 => LinkState::Blocked,
            _ => LinkState::Linked,
        }
    }
}

/// Edge `edge` of the host sketch lies on `face` of `wall`, shifted by `offset` to the left of
/// the wall direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeLink {
    pub edge: usize,
    pub wall: i64,
    pub face: LinkFace,
    pub offset: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinkedEdge {
    pub link: EdgeLink,
    pub state: LinkState,
}

/// Where a wall puts its linked edge.
enum Src {
    Gone,
    Bad,
    Line(V2, V2),
}

/// Line (point, unit direction start -> end) of `face` of a straight wall plus `offset`.
fn wall_line(db: &Connection, wall: i64, face: LinkFace, offset: f64) -> Result<Src, Error> {
    type Row = (f64, f64, f64, f64, f64, Option<f64>, i64);
    let row: Option<Row> = db
        .query_row(
            "SELECT ax, ay, bx, by, COALESCE(bulge, 0), offset0, wtype FROM wall WHERE id=?1",
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
    let Some((ax, ay, bx, by, bulge, off0, wtype)) = row else {
        return Ok(Src::Gone);
    };
    let (a, d) = (v2(ax, ay), v2(bx - ax, by - ay));
    let l = d.len();
    if bulge != 0.0 || l < TOL {
        return Ok(Src::Bad);
    }
    let u = d * (1.0 / l);
    let n = v2(-u.y, u.x);
    let t: f64 = db.query_row(
        "SELECT COALESCE(SUM(thickness), 0) FROM wall_type_layer WHERE wtype=?1",
        [wtype],
        |r| r.get(0),
    )?;
    let left = off0.unwrap_or(t / 2.0);
    let s = match face {
        LinkFace::Axis => 0.0,
        _ if t <= 0.0 => return Ok(Src::Bad),
        LinkFace::Left => left,
        LinkFace::Right => left - t,
    };
    Ok(Src::Line(a + n * (s + offset), u))
}

fn on_line(p: V2, (o, u): (V2, V2)) -> bool {
    (p - o).cross(u).abs() <= TOL
}

fn project(p: V2, (o, u): (V2, V2)) -> V2 {
    o + u * (p - o).dot(u)
}

fn meet((p1, d1): (V2, V2), (p2, d2): (V2, V2)) -> Option<V2> {
    let den = d1.cross(d2);
    (den.abs() > 1e-9).then(|| p1 + d1 * ((p2 - p1).cross(d2) / den))
}

/// Re-derives an outline: a linked edge lies on its line (`lines[i]`), a vertex it shares with
/// another straight edge is the intersection of both lines, free edges keep their own line.
/// `None`: not drawable (a moved corner meets a curve, or an edge would vanish or flip).
pub fn rebuild(sk: &Sketch, lines: &[Option<(V2, V2)>]) -> Option<Sketch> {
    let n = sk.curves.len();
    let linked = |i: usize| {
        matches!(sk.curves[i], SketchCurve::Line { .. })
            && lines.get(i).copied().flatten().is_some()
    };
    let own: Vec<Option<(V2, V2)>> = (0..n)
        .map(|i| match sk.curves[i] {
            SketchCurve::Line { a, b } => lines.get(i).copied().flatten().or_else(|| {
                let d = b - a;
                let l = d.len();
                (l > 0.0).then(|| (a, d * (1.0 / l)))
            }),
            _ => None,
        })
        .collect();
    let neighbour = |i: usize, p: V2| {
        (0..n).find(|&j| {
            let c = &sk.curves[j];
            j != i && ((c.start() - p).len() < TOL || (c.end() - p).len() < TOL)
        })
    };
    let vertex = |i: usize, p: V2| -> Option<V2> {
        let j = neighbour(i, p);
        let jl = j.is_some_and(|j| linked(j));
        if !linked(i) && !jl {
            return Some(p);
        }
        let li = own[i]?;
        match j.and_then(|j| own[j].map(|lj| (j, lj))) {
            Some((j, lj)) => Some(meet(li, lj).unwrap_or_else(|| {
                // collinear corner: stays where it was, on the linked line
                project(p, if linked(i) || !linked(j) { li } else { lj })
            })),
            None => {
                let q = project(p, li);
                ((q - p).len() <= TOL).then_some(p)
            }
        }
    };
    let mut out = sk.clone();
    for i in 0..n {
        let SketchCurve::Line { a, b } = sk.curves[i] else {
            continue;
        };
        let (na, nb) = (vertex(i, a)?, vertex(i, b)?);
        if (nb - na).dot(b - a) <= 0.0 || (nb - na).len() < 1e-3 {
            return None;
        }
        out.curves[i] = SketchCurve::Line { a: na, b: nb };
    }
    Some(out)
}

fn load(db: &Connection, kind: &str, id: i64) -> Result<Vec<LinkedEdge>, Error> {
    let mut st = db.prepare_cached(
        "SELECT edge, wall, face, offset, state FROM boundary_link \
         WHERE host_kind=?1 AND host=?2 ORDER BY edge",
    )?;
    let rows = st.query_map(params![kind, id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, f64>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    let mut out = vec![];
    for r in rows {
        let (edge, wall, face, offset, state) = r?;
        out.push(LinkedEdge {
            link: EdgeLink {
                edge: edge.max(0) as usize,
                wall,
                face: LinkFace::parse(&face).unwrap_or(LinkFace::Axis),
                offset,
            },
            state: LinkState::from_code(state),
        });
    }
    Ok(out)
}

/// Undo re-inserts every link row of the host after removing what the operation left.
fn snapshot(db: &Connection, inv: Inv, kind: &str, id: i64) -> Result<Inv, Error> {
    let mut inv = inv.sql(
        "DELETE FROM boundary_link WHERE host_kind=?1 AND host=?2",
        vec![kind.to_string().into(), id.into()],
    );
    for e in load(db, kind, id)? {
        inv = inv.sql(INSERT, row(kind, id, &e));
    }
    Ok(inv)
}

const INSERT: &str = "INSERT OR REPLACE INTO boundary_link(host_kind, host, edge, wall, face, \
    offset, state) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

fn row(kind: &str, id: i64, e: &LinkedEdge) -> Vec<Value> {
    vec![
        kind.to_string().into(),
        id.into(),
        (e.link.edge as i64).into(),
        e.link.wall.into(),
        e.link.face.key().to_string().into(),
        e.link.offset.into(),
        e.state.code().into(),
    ]
}

fn write_links(db: &Connection, kind: &str, id: i64, es: &[LinkedEdge]) -> Result<(), Error> {
    db.execute(
        "DELETE FROM boundary_link WHERE host_kind=?1 AND host=?2",
        params![kind, id],
    )?;
    for e in es {
        db.execute(INSERT, rusqlite::params_from_iter(row(kind, id, e)))?;
    }
    Ok(())
}

fn sketch_text_of(db: &Connection, kind: &str, id: i64) -> Result<Option<String>, Error> {
    Ok(db
        .query_row(
            "SELECT data FROM sketch WHERE host_kind=?1 AND host=?2",
            params![kind, id],
            |r| r.get(0),
        )
        .optional()?)
}

/// Writes the moved outline: sketch curves and the host vertices at the moved corners.
/// `false`: nothing written, the new outline is not valid for the host.
fn write_outline(
    db: &Connection,
    inv: &mut Inv,
    kind: &str,
    id: i64,
    old: &Sketch,
    new: &Sketch,
) -> Result<bool, Error> {
    let mut moves: Vec<(V2, V2)> = vec![];
    for (o, n) in old.curves.iter().zip(&new.curves) {
        for (p, q) in [(o.start(), n.start()), (o.end(), n.end())] {
            if (p - q).len() > 1e-12 && !moves.iter().any(|m| (m.0 - p).len() < TOL) {
                moves.push((p, q));
            }
        }
    }
    if moves.is_empty() {
        return Ok(true);
    }
    if new.validate().is_err() {
        return Ok(false);
    }
    let Some(&(_, table, owner, ring)) = host_of(kind) else {
        return Ok(false);
    };
    let extrusion = kind == "roof"
        && db
            .query_row("SELECT kind FROM roof WHERE id=?1", [id], |r| r.get::<_, String>(0))
            .optional()?
            .is_some_and(|k| k == "extrusion");
    // new corners in memory first: an invalid ring writes nothing
    let mut rows: Vec<(i64, i64, V2)> = vec![];
    let mut was: Vec<V2> = vec![];
    if !extrusion {
        let mut st = db.prepare(&format!(
            "SELECT rowid, {ring}, x, y FROM {table} WHERE {owner}=?1 ORDER BY {ring}, idx"
        ))?;
        let it = st.query_map([id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, v2(r.get(2)?, r.get(3)?)))
        })?;
        for r in it {
            let (rid, k, p) = r?;
            was.push(p);
            let q = moves
                .iter()
                .find(|m| (m.0 - p).len() < TOL)
                .map_or(p, |m| m.1);
            rows.push((rid, k, q));
        }
        let mut rings: Vec<Vec<(f64, f64)>> = vec![];
        let mut old: Vec<Vec<(f64, f64)>> = vec![];
        let mut last = None;
        for (&(_, k, q), p) in rows.iter().zip(&was) {
            if last != Some(k) {
                rings.push(vec![]);
                old.push(vec![]);
                last = Some(k);
            }
            if let (Some(r), Some(o)) = (rings.last_mut(), old.last_mut()) {
                r.push((q.x, q.y));
                o.push((p.x, p.y));
            }
        }
        if let Some((outer, holes)) = rings.split_first() {
            // a spike saved in the host follows its walls; a new one is refused
            if crate::slabs::profile_keeping(outer, holes, &old).is_err() {
                return Ok(false);
            }
        }
    }
    let old_text = sketch_text(old);
    *inv = std::mem::take(inv)
        .sql(
            "INSERT OR REPLACE INTO sketch(host_kind, host, data) VALUES (?1, ?2, ?3)",
            vec![kind.to_string().into(), id.into(), old_text.into()],
        )
        .restore(db, table, owner, id)?;
    db.execute(
        "UPDATE sketch SET data=?3 WHERE host_kind=?1 AND host=?2",
        params![kind, id, sketch_text(new)],
    )?;
    for (rid, _, q) in rows {
        db.execute(
            &format!("UPDATE {table} SET x=?2, y=?3 WHERE rowid=?1"),
            params![rid, q.x, q.y],
        )?;
    }
    Ok(true)
}

/// Re-derives the linked edges of one host and records state changes; its inverse goes in
/// front of `inv` (newest first).
fn refresh(db: &Connection, inv: Inv, kind: &str, id: i64) -> Result<Inv, Error> {
    let mut mine = Inv::default();
    let Some(text) = sketch_text_of(db, kind, id)? else {
        return Ok(inv);
    };
    let sk = parse_sketch(&text)?;
    let links = load(db, kind, id)?;
    if links.is_empty() {
        return Ok(inv);
    }
    let mut lines: Vec<Option<(V2, V2)>> = vec![None; sk.curves.len()];
    let mut states = vec![];
    for e in &links {
        let l = e.link;
        let line_edge = matches!(sk.curves.get(l.edge), Some(SketchCurve::Line { .. }));
        let st = match wall_line(db, l.wall, l.face, l.offset)? {
            Src::Gone => LinkState::SourceLost,
            Src::Line(p, u) if line_edge => {
                lines[l.edge] = Some((p, u));
                LinkState::Linked
            }
            _ => LinkState::Blocked,
        };
        states.push(st);
    }
    let ok = match rebuild(&sk, &lines) {
        Some(new) => write_outline(db, &mut mine, kind, id, &sk, &new)?,
        None => false,
    };
    for (e, st) in links.iter().zip(&mut states) {
        if !ok && *st == LinkState::Linked {
            *st = LinkState::Blocked;
        }
        if *st != e.state {
            mine = mine.sql(
                "UPDATE boundary_link SET state=?4 WHERE host_kind=?1 AND host=?2 AND edge=?3",
                vec![
                    kind.to_string().into(),
                    id.into(),
                    (e.link.edge as i64).into(),
                    e.state.code().into(),
                ],
            );
            db.execute(
                "UPDATE boundary_link SET state=?4 WHERE host_kind=?1 AND host=?2 AND edge=?3",
                params![kind, id, e.link.edge as i64, st.code()],
            )?;
        }
    }
    Ok(mine.then(inv))
}

/// Pass of `Model::op`: walls (or their types) changed, or a host was deleted. Linked edges
/// follow their walls; links of deleted hosts go. Same transaction, so the same undo step.
pub(crate) fn after_op(db: &Connection, t: &Touch, inv: Inv) -> Result<Inv, Error> {
    let walls = t.touched(&["wall", "wall_type_layer"]);
    if !walls && !t.deleted(&["slab", "roof", "ceiling", "shaft"]) {
        return Ok(inv);
    }
    let n: i64 = db.query_row("SELECT count(*) FROM boundary_link", [], |r| r.get(0))?;
    if n == 0 {
        return Ok(inv);
    }
    let mut mine = Inv::default();
    for &(kind, ..) in HOSTS {
        let gone: Vec<i64> = {
            let mut st = db.prepare(&format!(
                "SELECT DISTINCT host FROM boundary_link WHERE host_kind=?1 \
                 AND host NOT IN (SELECT id FROM {kind})"
            ))?;
            let v = st.query_map([kind], |r| r.get(0))?.collect::<Result<_, _>>()?;
            v
        };
        for h in gone {
            mine = snapshot(db, mine, kind, h)?;
            write_links(db, kind, h, &[])?;
        }
    }
    if walls {
        let hosts: Vec<(String, i64)> = {
            let mut st = db.prepare("SELECT DISTINCT host_kind, host FROM boundary_link")?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            v
        };
        for (kind, h) in hosts {
            mine = refresh(db, mine, &kind, h)?;
        }
    }
    // the follow-up edits are newest: undo reverts them first
    Ok(mine.then(inv))
}

/// Index of a straight curve lying on `line`: `prefer` first, then the first free one.
fn find_edge(sk: &Sketch, line: (V2, V2), prefer: usize, taken: &[usize]) -> Option<usize> {
    let fits = |i: usize| {
        !taken.contains(&i)
            && matches!(sk.curves.get(i), Some(SketchCurve::Line { a, b })
                if on_line(*a, line) && on_line(*b, line))
    };
    if fits(prefer) {
        return Some(prefer);
    }
    (0..sk.curves.len()).find(|&i| fits(i))
}

/// Part of «Done» in sketch mode: the host's links are re-matched to the new curves (an edge
/// moved off its wall, e.g. trimmed, is released), `given` links are added (replacing a link
/// on the same edge) and edges in `release` are unlinked. Returns the inverse and how many
/// links were released.
pub(crate) fn on_finish(
    db: &Connection,
    inv: Inv,
    (kind, id): (&str, i64),
    sk: &Sketch,
    given: &[EdgeLink],
    release: &[usize],
) -> Result<(Inv, usize), Error> {
    if host_of(kind).is_none() {
        return Ok((inv, 0));
    }
    let old = load(db, kind, id)?;
    if old.is_empty() && given.is_empty() {
        return Ok((inv, 0));
    }
    let inv = snapshot(db, inv, kind, id)?;
    let same = |a: &EdgeLink, b: &EdgeLink| {
        a.wall == b.wall && a.face == b.face && (a.offset - b.offset).abs() < TOL
    };
    let mut cand: Vec<LinkedEdge> = given
        .iter()
        .map(|&link| LinkedEdge {
            link,
            state: LinkState::Linked,
        })
        .collect();
    cand.extend(
        old.iter()
            .filter(|o| !given.iter().any(|g| same(g, &o.link) || g.edge == o.link.edge))
            .copied(),
    );
    let mut keep: Vec<LinkedEdge> = vec![];
    let mut released = 0;
    for mut e in cand {
        let taken: Vec<usize> = keep.iter().map(|k| k.link.edge).collect();
        let edge = match wall_line(db, e.link.wall, e.link.face, e.link.offset)? {
            Src::Line(p, u) => {
                e.state = LinkState::Linked;
                find_edge(sk, (p, u), e.link.edge, &taken)
            }
            // no wall line to match against: the edge keeps its index if it is still straight
            src => {
                e.state = match src {
                    Src::Gone => LinkState::SourceLost,
                    _ => LinkState::Blocked,
                };
                let i = e.link.edge;
                (matches!(sk.curves.get(i), Some(SketchCurve::Line { .. })) && !taken.contains(&i))
                    .then_some(i)
            }
        };
        match edge {
            Some(i) if !release.contains(&i) => {
                e.link.edge = i;
                keep.push(e);
            }
            _ => released += 1,
        }
    }
    write_links(db, kind, id, &keep)?;
    Ok((inv, released))
}

/// A linked edge on a plan level that needs attention (plan warning colour).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoundaryWarning {
    pub host: SketchHost,
    pub edge: usize,
    pub a: V2,
    pub b: V2,
    pub state: LinkState,
}

fn host_enum(kind: &str, id: i64) -> Option<SketchHost> {
    Some(match kind {
        "slab" => SketchHost::Slab(id),
        "roof" => SketchHost::Roof(id),
        "ceiling" => SketchHost::Ceiling(id),
        "shaft" => SketchHost::Shaft(id),
        _ => return None,
    })
}

fn sketch_host(host: SketchHost) -> Result<(&'static str, i64), Error> {
    let (kind, id) = host.key();
    if host_of(kind).is_none() {
        return Err(Error::Invalid(format!("{kind} outlines cannot link to walls")));
    }
    Ok((kind, id))
}

impl Model {
    /// «Done» with edges picked from walls: one undo step. Returns how many links (given or
    /// existing) were released because no edge lies on their wall any more.
    pub fn finish_linked_sketch(
        &mut self,
        host: SketchHost,
        sk: &Sketch,
        links: &[EdgeLink],
    ) -> Result<usize, Error> {
        self.finish_sketch_links(host, sk, links, &[])
    }

    /// «Done» of sketch mode: new links added, edges in `release` unlinked («Unlink» while
    /// sketching), the rest re-matched. One undo step; returns released links.
    pub fn finish_sketch_links(
        &mut self,
        host: SketchHost,
        sk: &Sketch,
        links: &[EdgeLink],
        release: &[usize],
    ) -> Result<usize, Error> {
        if !(links.is_empty() && release.is_empty()) {
            sketch_host(host)?;
        }
        self.finish_sketch_with(host, sk, links, release)
    }

    /// Linked edges of the host with their state (empty — none).
    pub fn boundary_links(&self, host: SketchHost) -> Result<Vec<LinkedEdge>, Error> {
        let (kind, id) = host.key();
        if host_of(kind).is_none() {
            return Ok(vec![]);
        }
        load(&self.db, kind, id)
    }

    /// Edges on `level` whose source wall was deleted or which cannot follow; one query when
    /// there are none (drawn every frame).
    pub fn boundary_warnings(&self, level: i64) -> Result<Vec<BoundaryWarning>, Error> {
        let rows: Vec<(String, i64, i64, i64)> = {
            let mut st = self.db.prepare_cached(
                "SELECT host_kind, host, edge, state FROM boundary_link WHERE state<>0 \
                 ORDER BY host_kind, host, edge",
            )?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<_, _>>()?;
            v
        };
        let mut out = vec![];
        for (kind, id, edge, state) in rows {
            let (Some(host), Some(_)) = (host_enum(&kind, id), host_of(&kind)) else {
                continue;
            };
            let lv: Option<i64> = self
                .db
                .query_row(&format!("SELECT level FROM {kind} WHERE id=?1"), [id], |r| {
                    r.get(0)
                })
                .optional()?;
            if lv != Some(level) {
                continue;
            }
            let Some(text) = sketch_text_of(&self.db, &kind, id)? else {
                continue;
            };
            let sk = parse_sketch(&text)?;
            if let Some(&SketchCurve::Line { a, b }) = sk.curves.get(edge.max(0) as usize) {
                out.push(BoundaryWarning {
                    host,
                    edge: edge.max(0) as usize,
                    a,
                    b,
                    state: LinkState::from_code(state),
                });
            }
        }
        Ok(out)
    }

    /// «Unlink»: the edges stay where they are and stop following (`edges` empty — all).
    /// Nothing linked is an error, so no empty undo step is recorded.
    pub fn unlink_boundary_edges(
        &mut self,
        host: SketchHost,
        edges: &[usize],
    ) -> Result<usize, Error> {
        let (kind, id) = sketch_host(host)?;
        self.drop_links(kind, id, "boundary_unlink", |e| {
            edges.is_empty() || edges.contains(&e.link.edge)
        })
    }

    /// Accepts edges whose source wall was deleted: they become ordinary free edges.
    pub fn accept_lost_edges(&mut self, host: SketchHost) -> Result<usize, Error> {
        let (kind, id) = sketch_host(host)?;
        self.drop_links(kind, id, "boundary_accept", |e| {
            e.state == LinkState::SourceLost
        })
    }

    fn drop_links(
        &mut self,
        kind: &'static str,
        id: i64,
        op: &str,
        pick: impl Fn(&LinkedEdge) -> bool,
    ) -> Result<usize, Error> {
        self.op(op, |db| {
            let all = load(db, kind, id)?;
            let rest: Vec<LinkedEdge> = all.iter().filter(|e| !pick(*e)).copied().collect();
            let n = all.len() - rest.len();
            if n == 0 {
                return Err(Error::Invalid("no linked edge to release".into()));
            }
            let inv = snapshot(db, Inv::default(), kind, id)?;
            write_links(db, kind, id, &rest)?;
            Ok((n, id, inv))
        })
    }

    /// «Re-pick»: edge `edge` follows `face` of `wall` from now on and moves onto it in the
    /// same step. Fails (nothing changes) if the outline cannot follow the new wall.
    pub fn relink_boundary_edge(
        &mut self,
        host: SketchHost,
        link: EdgeLink,
    ) -> Result<(), Error> {
        let (kind, id) = sketch_host(host)?;
        self.op(&format!("{kind}_sketch"), |db| {
            let text = sketch_text_of(db, kind, id)?
                .ok_or_else(|| Error::Invalid(format!("{kind} {id} has no sketch")))?;
            let sk = parse_sketch(&text)?;
            if !matches!(sk.curves.get(link.edge), Some(SketchCurve::Line { .. })) {
                return Err(Error::Invalid("only a straight edge can link to a wall".into()));
            }
            let mut es = load(db, kind, id)?;
            es.retain(|e| e.link.edge != link.edge);
            es.push(LinkedEdge {
                link,
                state: LinkState::Linked,
            });
            let inv = snapshot(db, Inv::default(), kind, id)?;
            write_links(db, kind, id, &es)?;
            let inv = refresh(db, inv, kind, id)?;
            let now = load(db, kind, id)?;
            if !now
                .iter()
                .any(|e| e.link.edge == link.edge && e.state == LinkState::Linked)
            {
                return Err(Error::Invalid("the outline cannot follow this wall".into()));
            }
            Ok(((), id, inv))
        })
    }

    /// Trim/extend: end `end` (0 start, 1 end) of edge `edge` moves to `to` together with the
    /// neighbour corner. Only this edge is released; other linked edges keep following.
    pub fn trim_boundary_edge(
        &mut self,
        host: SketchHost,
        edge: usize,
        end: usize,
        to: (f64, f64),
    ) -> Result<(), Error> {
        let (kind, id) = sketch_host(host)?;
        let to = v2(to.0, to.1);
        if !(to.x.is_finite() && to.y.is_finite()) || end > 1 {
            return Err(Error::Invalid("bad trim point".into()));
        }
        self.op(&format!("{kind}_sketch"), |db| {
            let text = sketch_text_of(db, kind, id)?
                .ok_or_else(|| Error::Invalid(format!("{kind} {id} has no sketch")))?;
            let old = parse_sketch(&text)?;
            let Some(&SketchCurve::Line { a, b }) = old.curves.get(edge) else {
                return Err(Error::Invalid("only a straight edge can be trimmed".into()));
            };
            let p = if end == 0 { a } else { b };
            let mut new = old.clone();
            for c in &mut new.curves {
                if let SketchCurve::Line { a, b } = c {
                    for q in [a, b] {
                        if (*q - p).len() < TOL {
                            *q = to;
                        }
                    }
                } else if (c.start() - p).len() < TOL || (c.end() - p).len() < TOL {
                    return Err(Error::Invalid("the corner meets a curve".into()));
                }
            }
            let mut es = load(db, kind, id)?;
            let mut inv = snapshot(db, Inv::default(), kind, id)?;
            es.retain(|e| e.link.edge != edge);
            write_links(db, kind, id, &es)?;
            if !write_outline(db, &mut inv, kind, id, &old, &new)? {
                return Err(Error::Invalid("the trimmed outline is not valid".into()));
            }
            let inv = refresh(db, inv, kind, id)?;
            Ok(((), id, inv))
        })
    }
}

#[cfg(test)]
#[path = "boundary_link_tests.rs"]
mod tests;
