//! `query_relations` and `query_topology`: read-only relation questions over the scratch copy.
//! Lengths are metres. Rows are ordered by category, then id, then a tag, and cut to `limit`;
//! the reply says how many more exist, so a model never pays for the whole model.

use super::fmt::{self, Reply};
use super::qspatial::{inside_ring, parse_ref};
use crate::editor::Editor;
use ddc_model::ConstraintRef as Ref;
use ddc_model::Boundary;
use serde_json::{json, Value as J};
use std::collections::BTreeMap;

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;
const TOPO_LIMIT: usize = 50;
const TOPO_MAX: usize = 200;
/// Metres: two points closer than this are the same point.
const TOL: f64 = 0.01;
const RELS: [&str; 7] = [
    "hosted_by",
    "hosts",
    "joined_to",
    "bounds_room",
    "in_room",
    "on_grid",
    "constrained_with",
];

type P = (f64, f64);

#[derive(Clone, Debug, PartialEq)]
pub struct W {
    pub id: i64,
    pub level: i64,
    pub a: P,
    pub b: P,
}

/// A door, window or void cut in a wall; `along` and `width` in metres from the axis start.
#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub id: i64,
    pub wall: i64,
    pub cat: &'static str,
    pub along: f64,
    pub width: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cm {
    pub id: i64,
    pub level: i64,
    pub at: P,
    pub wall: Option<i64>,
    pub slab: Option<i64>,
    pub ceiling: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gr {
    pub id: i64,
    pub a: P,
    pub b: P,
}

/// A room with its boundary rings (outer first) and what closes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Rm {
    pub id: i64,
    pub level: i64,
    pub rings: Vec<Vec<P>>,
    pub walls: Vec<i64>,
    pub seps: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub id: i64,
    pub kind: &'static str,
    pub value: Option<f64>,
    pub parts: Vec<(&'static str, i64)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rel {
    pub walls: Vec<W>,
    pub ops: Vec<Op>,
    pub comps: Vec<Cm>,
    pub grids: Vec<Gr>,
    pub rooms: Vec<Rm>,
    pub rules: Vec<Rule>,
}

fn sql_err(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Everything the relations need, read in id order.
pub fn rel_of(ed: &Editor) -> Result<Rel, String> {
    let m = &ed.model;
    let db = m.sql();
    let mut r = Rel::default();
    let mut st = db
        .prepare("SELECT id, level, ax, ay, bx, by FROM wall ORDER BY id")
        .map_err(sql_err)?;
    let rows = st
        .query_map([], |x| {
            Ok(W {
                id: x.get(0)?,
                level: x.get(1)?,
                a: (x.get(2)?, x.get(3)?),
                b: (x.get(4)?, x.get(5)?),
            })
        })
        .map_err(sql_err)?;
    for w in rows {
        r.walls.push(w.map_err(sql_err)?);
    }
    let mut st = db
        .prepare("SELECT id, wall, along, width, kind FROM opening ORDER BY id")
        .map_err(sql_err)?;
    let rows = st
        .query_map([], |x| {
            let kind: String = x.get(4)?;
            Ok(Op {
                id: x.get(0)?,
                wall: x.get(1)?,
                cat: match kind.as_str() {
                    "door" => "door",
                    "window" => "window",
                    _ => "opening",
                },
                along: x.get(2)?,
                width: x.get(3)?,
            })
        })
        .map_err(sql_err)?;
    for o in rows {
        r.ops.push(o.map_err(sql_err)?);
    }
    let mut st = db
        .prepare("SELECT id, level, x, y, host_wall, host_slab, host_ceiling FROM comp ORDER BY id")
        .map_err(sql_err)?;
    let rows = st
        .query_map([], |x| {
            Ok(Cm {
                id: x.get(0)?,
                level: x.get(1)?,
                at: (x.get(2)?, x.get(3)?),
                wall: x.get(4)?,
                slab: x.get(5)?,
                ceiling: x.get(6)?,
            })
        })
        .map_err(sql_err)?;
    for c in rows {
        r.comps.push(c.map_err(sql_err)?);
    }
    for g in m.grids().map_err(|e| e.to_string())? {
        // an arc grid has no single line: it relates to nothing here
        if g.spec.bulge == 0.0 {
            r.grids.push(Gr {
                id: g.id,
                a: g.spec.a,
                b: g.spec.b,
            });
        }
    }
    r.grids.sort_by_key(|g| g.id);
    let mut st = db
        .prepare("SELECT id, level FROM room ORDER BY id")
        .map_err(sql_err)?;
    let rooms: Vec<(i64, i64)> = st
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))
        .map_err(sql_err)?
        .collect::<Result<_, _>>()
        .map_err(sql_err)?;
    for (id, level) in rooms {
        // an open room has no boundary: it still exists, with nothing around it
        let rings = m
            .room_quantities(id)
            .map_err(|e| e.to_string())?
            .map_or(vec![], |q| q.rings);
        let (mut walls, mut seps) = (vec![], vec![]);
        for b in m.room_boundaries(id).map_err(|e| e.to_string())? {
            match b {
                Boundary::Wall(w) => walls.push(w),
                Boundary::Sep(s) => seps.push(s),
            }
        }
        r.rooms.push(Rm {
            id,
            level,
            rings,
            walls,
            seps,
        });
    }
    for (id, c) in m.constraints().map_err(|e| e.to_string())? {
        let parts = c
            .refs()
            .into_iter()
            .map(|p| match p {
                Ref::Wall(i) => ("wall", i),
                Ref::Grid(i) => ("grid", i),
                Ref::Level(i) => ("level", i),
            })
            .collect();
        r.rules.push(Rule {
            id,
            kind: c.kind.code(),
            value: c.value,
            parts,
        });
    }
    r.rules.sort_by_key(|x| x.id);
    Ok(r)
}

fn dist(p: P, q: P) -> f64 {
    ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt()
}

fn cross(u: P, v: P) -> f64 {
    u.0 * v.1 - u.1 * v.0
}

fn sub(p: P, q: P) -> P {
    (p.0 - q.0, p.1 - q.1)
}

/// Distance from `p` to the segment `a`-`b`.
fn seg_dist(p: P, a: P, b: P) -> f64 {
    let d = sub(b, a);
    let l2 = d.0 * d.0 + d.1 * d.1;
    if l2 < 1e-18 {
        return dist(p, a);
    }
    let t = (((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1) / l2).clamp(0.0, 1.0);
    dist(p, (a.0 + t * d.0, a.1 + t * d.1))
}

/// `p` lies on the segment and away from both of its ends.
fn on_interior(p: P, a: P, b: P) -> bool {
    seg_dist(p, a, b) <= TOL && dist(p, a) > TOL && dist(p, b) > TOL
}

/// The crossing point of two segments that cross inside both, away from all four ends.
fn crossing(a: P, b: P, c: P, d: P) -> Option<P> {
    let (d1, d2) = (sub(b, a), sub(d, c));
    let den = cross(d1, d2);
    if den.abs() < 1e-12 {
        return None;
    }
    let ca = sub(c, a);
    let (t, u) = (cross(ca, d2) / den, cross(ca, d1) / den);
    if !(0.0..=1.0).contains(&t) || !(0.0..=1.0).contains(&u) {
        return None;
    }
    let p = (a.0 + t * d1.0, a.1 + t * d1.1);
    [a, b, c, d]
        .iter()
        .all(|e| dist(p, *e) > TOL)
        .then_some(p)
}

fn end_name(i: usize) -> &'static str {
    ["a", "b"][i]
}

/// One row of an answer: sorted by (cat, id, tag); `extra` holds the named numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct RHit {
    pub cat: &'static str,
    pub id: i64,
    pub tag: String,
    pub extra: J,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub rel: &'static str,
    pub total: usize,
    pub hits: Vec<RHit>,
}

fn hit(cat: &'static str, id: i64) -> RHit {
    RHit {
        cat,
        id,
        tag: String::new(),
        extra: json!({}),
    }
}

fn pt(p: P) -> J {
    json!([fmt::num(p.0), fmt::num(p.1)])
}

fn ref_text(cat: &str, id: i64) -> String {
    format!("{cat}:{id}")
}

impl Rel {
    fn wall(&self, id: i64) -> Option<&W> {
        self.walls.iter().find(|w| w.id == id)
    }

    fn op(&self, cat: &str, id: i64) -> Option<&Op> {
        self.ops.iter().find(|o| o.id == id && o.cat == cat)
    }

    /// Level and a representative point of an element: wall middle, opening centre, placement.
    fn point_of(&self, cat: &str, id: i64) -> Option<(i64, P)> {
        match cat {
            "wall" => {
                let w = self.wall(id)?;
                Some((w.level, ((w.a.0 + w.b.0) / 2.0, (w.a.1 + w.b.1) / 2.0)))
            }
            "door" | "window" | "opening" => {
                let o = self.op(cat, id)?;
                let w = self.wall(o.wall)?;
                let len = dist(w.a, w.b);
                let t = if len < 1e-9 {
                    0.0
                } else {
                    ((o.along + o.width / 2.0) / len).clamp(0.0, 1.0)
                };
                Some((
                    w.level,
                    (w.a.0 + t * (w.b.0 - w.a.0), w.a.1 + t * (w.b.1 - w.a.1)),
                ))
            }
            "comp" => self.comps.iter().find(|c| c.id == id).map(|c| (c.level, c.at)),
            _ => None,
        }
    }

    /// How wall `w` meets the other walls of its level.
    fn joins(&self, w: &W) -> Vec<RHit> {
        let mut out = vec![];
        for o in self.walls.iter().filter(|o| o.id != w.id && o.level == w.level) {
            let mut found = false;
            for (i, p) in [w.a, w.b].into_iter().enumerate() {
                let kind = if dist(p, o.a) <= TOL || dist(p, o.b) <= TOL {
                    "L"
                } else if on_interior(p, o.a, o.b) {
                    "T"
                } else {
                    continue;
                };
                found = true;
                let mut h = hit("wall", o.id);
                h.tag = format!("{}{kind}", end_name(i));
                h.extra = json!({"join": kind, "end": end_name(i), "at": pt(p)});
                out.push(h);
            }
            for p in [o.a, o.b] {
                if on_interior(p, w.a, w.b) {
                    found = true;
                    let mut h = hit("wall", o.id);
                    h.tag = "mT".to_string();
                    h.extra = json!({"join": "T", "end": "mid", "at": pt(p)});
                    out.push(h);
                }
            }
            if !found {
                if let Some(p) = crossing(w.a, w.b, o.a, o.b) {
                    let mut h = hit("wall", o.id);
                    h.tag = "mX".to_string();
                    h.extra = json!({"join": "X", "end": "mid", "at": pt(p)});
                    out.push(h);
                }
            }
        }
        out
    }

    /// Wall ends that touch no other wall, neither at an end nor along its length.
    fn open_ends(&self, level: i64) -> Vec<(i64, usize, P)> {
        let mut out = vec![];
        for w in self.walls.iter().filter(|w| w.level == level) {
            let joins = self.joins(w);
            for (i, p) in [w.a, w.b].into_iter().enumerate() {
                let e = end_name(i);
                if !joins.iter().any(|h| h.extra["end"] == e) {
                    out.push((w.id, i, p));
                }
            }
        }
        out
    }
}

fn parse_rel_ref(a: &J) -> Result<(&'static str, &'static str, i64), String> {
    let s = a["ref"].as_str().ok_or("ref must be text like wall:12")?;
    let (cat, id) = parse_ref(s)?;
    let rel = a["rel"].as_str().unwrap_or("");
    let rel = RELS
        .iter()
        .copied()
        .find(|r| *r == rel)
        .ok_or_else(|| format!("rel must be one of {}", RELS.join(", ")))?;
    let cat = [
        "wall", "door", "window", "opening", "comp", "floor", "ceiling", "room", "grid", "level",
    ]
    .into_iter()
    .find(|c| *c == cat)
    .ok_or_else(|| format!("unknown category in {}", fmt::text(s)))?;
    Ok((rel, cat, id))
}

fn limit_of(a: &J, default: usize, max: usize) -> usize {
    a["limit"]
        .as_u64()
        .map_or(default, |n| (n as usize).clamp(1, max))
}

/// Runs one relation question. `a.limit` (default 20, at most 100) cuts the rows; `total` counts all.
pub fn run_relations(r: &Rel, a: &J) -> Result<Found, String> {
    let (rel, cat, id) = parse_rel_ref(a)?;
    let nope = || format!("{rel} does not apply to {cat}");
    let missing = || format!("no such element: {}", ref_text(cat, id));
    let mut hits: Vec<RHit> = vec![];
    match rel {
        "hosted_by" => match cat {
            "door" | "window" | "opening" => {
                let o = r.op(cat, id).ok_or_else(missing)?;
                hits.push(hit("wall", o.wall));
            }
            "comp" => {
                let c = r.comps.iter().find(|c| c.id == id).ok_or_else(missing)?;
                hits.extend(c.wall.map(|w| hit("wall", w)));
                hits.extend(c.slab.map(|s| hit("floor", s)));
                hits.extend(c.ceiling.map(|s| hit("ceiling", s)));
            }
            _ => return Err(nope()),
        },
        "hosts" => match cat {
            "wall" => {
                let w = r.wall(id).ok_or_else(missing)?;
                for o in r.ops.iter().filter(|o| o.wall == w.id) {
                    let mut h = hit(o.cat, o.id);
                    h.extra = json!({"along": fmt::num(o.along), "width": fmt::num(o.width)});
                    hits.push(h);
                }
                hits.extend(r.comps.iter().filter(|c| c.wall == Some(w.id)).map(|c| hit("comp", c.id)));
            }
            "floor" => hits.extend(r.comps.iter().filter(|c| c.slab == Some(id)).map(|c| hit("comp", c.id))),
            "ceiling" => hits.extend(r.comps.iter().filter(|c| c.ceiling == Some(id)).map(|c| hit("comp", c.id))),
            _ => return Err(nope()),
        },
        "joined_to" => {
            if cat != "wall" {
                return Err(nope());
            }
            hits = r.joins(r.wall(id).ok_or_else(missing)?);
        }
        "bounds_room" => match cat {
            "wall" => {
                r.wall(id).ok_or_else(missing)?;
                hits.extend(r.rooms.iter().filter(|m| m.walls.contains(&id)).map(|m| hit("room", m.id)));
            }
            // the other way round: what closes the room
            "room" => {
                let m = r.rooms.iter().find(|m| m.id == id).ok_or_else(missing)?;
                hits.extend(m.walls.iter().map(|w| hit("wall", *w)));
                hits.extend(m.seps.iter().map(|s| hit("room_sep", *s)));
            }
            _ => return Err(nope()),
        },
        "in_room" => {
            let (level, p) = r.point_of(cat, id).ok_or_else(|| {
                if matches!(cat, "wall" | "door" | "window" | "opening" | "comp") {
                    missing()
                } else {
                    nope()
                }
            })?;
            hits.extend(
                r.rooms
                    .iter()
                    .filter(|m| m.level == level && inside_ring(&m.rings, p.0, p.1))
                    .map(|m| hit("room", m.id)),
            );
        }
        "on_grid" => match cat {
            "wall" => {
                let w = r.wall(id).ok_or_else(missing)?;
                for g in r.grids.iter().filter(|g| on_line(w, g)) {
                    let mut h = hit("grid", g.id);
                    h.extra = json!({"offset": 0});
                    hits.push(h);
                }
            }
            "grid" => {
                let g = r.grids.iter().find(|g| g.id == id).ok_or_else(missing)?;
                hits.extend(r.walls.iter().filter(|w| on_line(w, g)).map(|w| hit("wall", w.id)));
            }
            _ => return Err(nope()),
        },
        _ => {
            if !matches!(cat, "wall" | "grid" | "level") {
                return Err(nope());
            }
            for u in r.rules.iter().filter(|u| u.parts.contains(&(cat, id))) {
                let others: Vec<String> = u
                    .parts
                    .iter()
                    .filter(|p| **p != (cat, id))
                    .map(|p| ref_text(p.0, p.1))
                    .collect();
                let mut h = hit("rule", u.id);
                h.extra = json!({"kind": u.kind, "with": others});
                if let Some(v) = u.value {
                    h.extra["value"] = fmt::num(v);
                }
                hits.push(h);
            }
        }
    }
    hits.sort_by(|x, y| {
        x.cat
            .cmp(y.cat)
            .then(x.id.cmp(&y.id))
            .then_with(|| x.tag.cmp(&y.tag))
    });
    let limit = limit_of(a, DEFAULT_LIMIT, MAX_LIMIT);
    let total = hits.len();
    hits.truncate(limit);
    Ok(Found { rel, total, hits })
}

/// Both ends of the wall lie on the infinite line of the grid, and the two are parallel.
fn on_line(w: &W, g: &Gr) -> bool {
    let d = sub(g.b, g.a);
    let l = (d.0 * d.0 + d.1 * d.1).sqrt();
    if l < 1e-9 {
        return false;
    }
    [w.a, w.b]
        .iter()
        .all(|p| (cross(d, sub(*p, g.a)) / l).abs() <= TOL)
}

pub fn answer_relations(r: &Rel, a: &J, max_chars: usize) -> Result<Reply, String> {
    let f = run_relations(r, a)?;
    let rows: Vec<J> = f
        .hits
        .iter()
        .map(|h| {
            let mut v = json!({"ref": ref_text(h.cat, h.id)});
            if let (Some(o), Some(e)) = (v.as_object_mut(), h.extra.as_object()) {
                for (k, x) in e {
                    o.insert(k.clone(), x.clone());
                }
            }
            v
        })
        .collect();
    let mut o = fmt::Out::new(max_chars);
    o.put("units", &json!({"length": "m"}));
    o.put("rel", &json!(f.rel));
    o.put("total", &json!(f.total));
    o.put("returned", &json!(rows.len()));
    o.put("more", &json!(f.total - rows.len()));
    o.list("rows", &rows);
    Ok(o.finish())
}

/// A point where wall ends meet, and the walls that end there.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub n: usize,
    pub at: P,
    pub ends: Vec<(i64, usize)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Adj {
    pub a: i64,
    pub b: i64,
    pub walls: Vec<i64>,
    pub seps: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Topo {
    pub level: i64,
    pub walls: usize,
    pub nodes: Vec<Node>,
    pub open: Vec<(i64, usize, P)>,
    pub adj: Vec<Adj>,
}

/// Wall graph of `level`: nodes are numbered in order of first appearance by wall id, then end.
pub fn topology(r: &Rel, level: i64) -> Topo {
    let ws: Vec<&W> = r.walls.iter().filter(|w| w.level == level).collect();
    let mut nodes: Vec<Node> = vec![];
    for w in &ws {
        for (i, p) in [w.a, w.b].into_iter().enumerate() {
            match nodes.iter_mut().find(|n| dist(n.at, p) <= TOL) {
                Some(n) => n.ends.push((w.id, i)),
                None => nodes.push(Node {
                    n: nodes.len() + 1,
                    at: p,
                    ends: vec![(w.id, i)],
                }),
            }
        }
    }
    let mut pair: BTreeMap<(i64, i64), Adj> = BTreeMap::new();
    let rooms: Vec<&Rm> = r.rooms.iter().filter(|m| m.level == level).collect();
    for (k, m) in rooms.iter().enumerate() {
        for n in &rooms[k + 1..] {
            let walls: Vec<i64> = m.walls.iter().copied().filter(|w| n.walls.contains(w)).collect();
            let seps: Vec<i64> = m.seps.iter().copied().filter(|s| n.seps.contains(s)).collect();
            if !walls.is_empty() || !seps.is_empty() {
                let (a, b) = (m.id.min(n.id), m.id.max(n.id));
                pair.insert((a, b), Adj { a, b, walls, seps });
            }
        }
    }
    Topo {
        level,
        walls: ws.len(),
        nodes,
        open: r.open_ends(level),
        adj: pair.into_values().collect(),
    }
}

pub fn answer_topology(r: &Rel, a: &J, max_chars: usize) -> Result<Reply, String> {
    let level = a["level"].as_i64().ok_or("level must be a level id")?;
    let t = topology(r, level);
    let limit = limit_of(a, TOPO_LIMIT, TOPO_MAX);
    let mut o = fmt::Out::new(max_chars);
    o.put("units", &json!({"length": "m"}));
    o.put("level", &json!(t.level));
    o.put("walls", &json!(t.walls));
    o.put("nodes_total", &json!(t.nodes.len()));
    o.put("open_ends_total", &json!(t.open.len()));
    o.put("adjacent_pairs_total", &json!(t.adj.len()));
    let cut = |n: usize| n.min(limit);
    let nodes: Vec<J> = t.nodes.iter().take(cut(t.nodes.len())).map(|n| {
        let ends: Vec<J> = n.ends.iter().map(|(w, i)| json!(format!("wall:{w}.{}", end_name(*i)))).collect();
        json!({"node": n.n, "at": pt(n.at), "ends": ends})
    }).collect();
    let open: Vec<J> = t.open.iter().take(cut(t.open.len())).map(|(w, i, p)| {
        json!({"ref": format!("wall:{w}"), "end": end_name(*i), "at": pt(*p)})
    }).collect();
    let adj: Vec<J> = t.adj.iter().take(cut(t.adj.len())).map(|x| {
        let walls: Vec<String> = x.walls.iter().map(|w| format!("wall:{w}")).collect();
        let seps: Vec<String> = x.seps.iter().map(|s| format!("room_sep:{s}")).collect();
        json!({"a": format!("room:{}", x.a), "b": format!("room:{}", x.b), "walls": walls, "seps": seps})
    }).collect();
    o.put("more", &json!({
        "nodes": t.nodes.len() - nodes.len(),
        "open_ends": t.open.len() - open.len(),
        "adjacent_pairs": t.adj.len() - adj.len(),
    }));
    o.list("open_ends", &open);
    o.list("adjacent", &adj);
    o.list("nodes", &nodes);
    Ok(o.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(id: i64, a: P, b: P) -> W {
        W { id, level: 1, a, b }
    }

    fn square(id: i64, x0: f64, x1: f64) -> Rm {
        Rm {
            id,
            level: 1,
            rings: vec![vec![(x0, 0.0), (x1, 0.0), (x1, 4.0), (x0, 4.0)]],
            walls: vec![],
            seps: vec![],
        }
    }

    fn refs(f: &Found) -> Vec<String> {
        f.hits.iter().map(|h| ref_text(h.cat, h.id)).collect()
    }

    fn q(r: &str, rel: &str) -> J {
        json!({"ref": r, "rel": rel})
    }

    #[test]
    fn wall_hosts_lists_openings() {
        let r = Rel {
            walls: vec![wall(1, (0.0, 0.0), (10.0, 0.0)), wall(2, (0.0, 5.0), (10.0, 5.0))],
            ops: vec![
                Op { id: 9, wall: 1, cat: "window", along: 5.0, width: 1.2 },
                Op { id: 4, wall: 1, cat: "door", along: 1.0, width: 0.9 },
                Op { id: 5, wall: 2, cat: "door", along: 1.0, width: 0.9 },
            ],
            comps: vec![Cm { id: 3, level: 1, at: (2.0, 0.0), wall: Some(1), slab: None, ceiling: None }],
            ..Rel::default()
        };
        let f = run_relations(&r, &q("wall:1", "hosts")).unwrap();
        assert_eq!(refs(&f), vec!["comp:3", "door:4", "window:9"]);
        assert_eq!(f.hits[1].extra["along"], 1);
        let f = run_relations(&r, &q("window:9", "hosted_by")).unwrap();
        assert_eq!(refs(&f), vec!["wall:1"]);
        let f = run_relations(&r, &q("comp:3", "hosted_by")).unwrap();
        assert_eq!(refs(&f), vec!["wall:1"]);
        assert!(run_relations(&r, &q("window:4", "hosted_by")).is_err());
    }

    #[test]
    fn room_bounding_walls_ids() {
        let mut m = square(7, 0.0, 4.0);
        m.walls = vec![3, 1, 2];
        m.seps = vec![5];
        let r = Rel {
            walls: vec![wall(1, (0.0, 0.0), (4.0, 0.0)), wall(8, (9.0, 0.0), (9.0, 4.0))],
            rooms: vec![m],
            ..Rel::default()
        };
        let f = run_relations(&r, &q("room:7", "bounds_room")).unwrap();
        assert_eq!(refs(&f), vec!["room_sep:5", "wall:1", "wall:2", "wall:3"]);
        let f = run_relations(&r, &q("wall:1", "bounds_room")).unwrap();
        assert_eq!(refs(&f), vec!["room:7"]);
        assert!(run_relations(&r, &q("wall:8", "bounds_room")).unwrap().hits.is_empty());
        assert!(run_relations(&r, &q("wall:99", "bounds_room")).is_err());
    }

    #[test]
    fn room_adjacency_shared_wall() {
        let mut a = square(1, 0.0, 4.0);
        a.walls = vec![1, 2, 3, 4];
        let mut b = square(2, 4.0, 8.0);
        b.walls = vec![3, 5, 6, 7];
        let mut c = square(3, 8.0, 12.0);
        c.walls = vec![8, 9];
        let mut far = square(4, 0.0, 4.0);
        far.level = 2;
        far.walls = vec![3];
        let r = Rel {
            rooms: vec![c, b, a, far],
            ..Rel::default()
        };
        let t = topology(&r, 1);
        assert_eq!(t.adj.len(), 1);
        assert_eq!((t.adj[0].a, t.adj[0].b, t.adj[0].walls.clone()), (1, 2, vec![3]));
        let v: J = serde_json::from_str(&answer_topology(&r, &json!({"level": 1}), 4000).unwrap().text).unwrap();
        assert_eq!(v["adjacent"][0]["walls"][0], "wall:3");
        assert_eq!(v["adjacent_pairs_total"], 1);
    }

    #[test]
    fn open_wall_end_reported() {
        // 1 and 2 meet at a corner; 3 butts into the middle of 1; 4 floats free
        let r = Rel {
            walls: vec![
                wall(1, (0.0, 0.0), (10.0, 0.0)),
                wall(2, (10.0, 0.0), (10.0, 6.0)),
                wall(3, (5.0, 0.0), (5.0, 4.0)),
                wall(4, (20.0, 0.0), (25.0, 0.0)),
            ],
            ..Rel::default()
        };
        let t = topology(&r, 1);
        let open: Vec<(i64, usize)> = t.open.iter().map(|o| (o.0, o.1)).collect();
        assert_eq!(open, vec![(1, 0), (2, 1), (3, 1), (4, 0), (4, 1)]);
        let f = run_relations(&r, &q("wall:1", "joined_to")).unwrap();
        assert_eq!(refs(&f), vec!["wall:2", "wall:3"]);
        assert_eq!(f.hits[0].extra["join"], "L");
        assert_eq!(f.hits[0].extra["end"], "b");
        assert_eq!(f.hits[1].extra["join"], "T");
        assert_eq!(f.hits[1].extra["end"], "mid");
        let f = run_relations(&r, &q("wall:3", "joined_to")).unwrap();
        assert_eq!((f.hits[0].extra["join"].clone(), f.hits[0].extra["end"].clone()), (json!("T"), json!("a")));
        // a crossing is X
        let x = Rel {
            walls: vec![wall(1, (0.0, 0.0), (10.0, 0.0)), wall(2, (5.0, -3.0), (5.0, 3.0))],
            ..Rel::default()
        };
        let f = run_relations(&x, &q("wall:1", "joined_to")).unwrap();
        assert_eq!(f.hits[0].extra["join"], "X");
    }

    #[test]
    fn in_room_grid_and_rules() {
        let r = Rel {
            walls: vec![wall(1, (1.0, 1.0), (3.0, 1.0)), wall(2, (6.0, 1.0), (7.0, 1.0))],
            grids: vec![Gr { id: 2, a: (0.0, 1.0), b: (20.0, 1.0) }, Gr { id: 3, a: (0.0, 2.0), b: (20.0, 2.0) }],
            rooms: vec![square(7, 0.0, 4.0)],
            rules: vec![Rule {
                id: 6,
                kind: "lock_dist_grid",
                value: Some(1.0),
                parts: vec![("wall", 1), ("grid", 2)],
            }],
            ..Rel::default()
        };
        assert_eq!(refs(&run_relations(&r, &q("wall:1", "in_room")).unwrap()), vec!["room:7"]);
        assert!(run_relations(&r, &q("wall:2", "in_room")).unwrap().hits.is_empty());
        assert_eq!(refs(&run_relations(&r, &q("wall:1", "on_grid")).unwrap()), vec!["grid:2"]);
        assert_eq!(refs(&run_relations(&r, &q("grid:2", "on_grid")).unwrap()), vec!["wall:1", "wall:2"]);
        let f = run_relations(&r, &q("grid:2", "constrained_with")).unwrap();
        assert_eq!(refs(&f), vec!["rule:6"]);
        assert_eq!(f.hits[0].extra["with"][0], "wall:1");
        assert_eq!(f.hits[0].extra["kind"], "lock_dist_grid");
    }

    #[test]
    fn order_is_deterministic() {
        let a = Rel {
            walls: vec![
                wall(5, (0.0, 0.0), (4.0, 0.0)),
                wall(9, (4.0, 0.0), (4.0, 4.0)),
                wall(2, (0.0, 0.0), (0.0, 4.0)),
            ],
            ..Rel::default()
        };
        let mut b = a.clone();
        b.walls.reverse();
        let qq = q("wall:5", "joined_to");
        assert_eq!(run_relations(&a, &qq).unwrap(), run_relations(&b, &qq).unwrap());
        assert_eq!(refs(&run_relations(&a, &qq).unwrap()), vec!["wall:2", "wall:9"]);
    }

    #[test]
    fn limit_and_more_count() {
        let r = Rel {
            walls: vec![wall(1, (0.0, 0.0), (10.0, 0.0))],
            ops: (1..=30).map(|i| Op { id: i, wall: 1, cat: "door", along: i as f64 * 0.3, width: 0.1 }).collect(),
            ..Rel::default()
        };
        let a = json!({"ref": "wall:1", "rel": "hosts", "limit": 5});
        let f = run_relations(&r, &a).unwrap();
        assert_eq!((f.total, f.hits.len()), (30, 5));
        let v: J = serde_json::from_str(&answer_relations(&r, &a, 4000).unwrap().text).unwrap();
        assert_eq!((v["returned"].as_u64(), v["more"].as_u64()), (Some(5), Some(25)));
        assert_eq!(v["units"]["length"], "m");
        assert_eq!(v["rows"][0]["ref"], "door:1");
        assert_eq!(run_relations(&r, &q("wall:1", "hosts")).unwrap().hits.len(), 20);
    }

    #[test]
    fn empty_model() {
        let ed = Editor::new_in_memory().unwrap();
        let r = rel_of(&ed).unwrap();
        assert!(run_relations(&r, &q("wall:1", "joined_to")).is_err());
        assert!(run_relations(&r, &json!({"ref": "wall:1", "rel": "nope"})).is_err());
        let t: J = serde_json::from_str(&answer_topology(&r, &json!({"level": 1}), 2000).unwrap().text).unwrap();
        assert_eq!((t["nodes_total"].as_u64(), t["open_ends_total"].as_u64()), (Some(0), Some(0)));
        assert!(t["adjacent"].as_array().unwrap().is_empty());
        assert!(answer_topology(&r, &json!({}), 2000).is_err());
    }
}
