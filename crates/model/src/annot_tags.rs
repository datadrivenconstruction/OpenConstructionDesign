// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tags as elements: a tag of a door/window/room/wall/component is a `dim` row with kind
//! `tag`, refs `H <k> <id>` (k: w o r c), (dx, dy) is the head offset from the host point, side 1 means
//! with a leader. The position is computed on read from the current host, so the tag follows it.
//! Deleting the host deletes its tags in the same operation ([`cascade`]). A door/window number is
//! `prop(kind 'opening', key 'mark')`, a room number is `room.number`.
//! This file also holds "tag all" (non-overlapping layout) and dimensions for selected walls.

use std::collections::{BTreeMap, BTreeSet};

use ddc_geom::dim;
use ddc_geom::{v2, V2};
use rusqlite::{params, types::Value, Connection, OptionalExtension};

use super::{refs_text, Ctx, DimKind, DimRef, DimSpec, P};
use crate::ops::Inv;
use crate::{Error, Model};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TagHost {
    Wall(i64),
    Opening(i64),
    Room(i64),
    Comp(i64),
}

impl TagHost {
    fn table(self) -> &'static str {
        match self {
            TagHost::Wall(_) => "wall",
            TagHost::Opening(_) => "opening",
            TagHost::Room(_) => "room",
            TagHost::Comp(_) => "comp",
        }
    }
    pub fn id(self) -> i64 {
        match self {
            TagHost::Wall(i) | TagHost::Opening(i) | TagHost::Room(i) | TagHost::Comp(i) => i,
        }
    }
    fn text(self) -> String {
        let k = match self {
            TagHost::Wall(_) => 'w',
            TagHost::Opening(_) => 'o',
            TagHost::Room(_) => 'r',
            TagHost::Comp(_) => 'c',
        };
        format!("H {k} {}", self.id())
    }
    fn parse(s: &str) -> Option<Self> {
        let v: Vec<&str> = s.split(' ').collect();
        let [h, k, id] = v[..] else { return None };
        if h != "H" {
            return None;
        }
        let id = id.parse().ok()?;
        Some(match k {
            "w" => TagHost::Wall(id),
            "o" => TagHost::Opening(id),
            "r" => TagHost::Room(id),
            "c" => TagHost::Comp(id),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TagCat {
    Door,
    Window,
    Room,
    Wall,
    Comp,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TagSpec {
    pub level: i64,
    pub host: TagHost,
    /// Tag head relative to the host point, m.
    pub offset: P,
    pub leader: bool,
}

/// Tag text: number/mark (empty means unset; the UI shows a placeholder), name, room area.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TagText {
    pub mark: String,
    pub name: String,
    pub area: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    pub id: i64,
    pub spec: TagSpec,
    pub cat: TagCat,
    /// Host point (opening centre on the axis, wall axis midpoint, room point).
    pub anchor: P,
    pub head: P,
    pub text: TagText,
}

/// Host in plan: category, level, point, wall normal (for openings and walls), text.
struct HostInfo {
    cat: TagCat,
    level: i64,
    anchor: V2,
    normal: V2,
    along: V2,
    text: TagText,
}

fn val_text(v: Value) -> String {
    match v {
        Value::Text(s) => s,
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        _ => String::new(),
    }
}

fn mark_of(db: &Connection, opening: i64) -> Result<String, Error> {
    Ok(db
        .prepare_cached("SELECT value FROM prop WHERE kind='opening' AND ref=?1 AND key='mark'")?
        .query_row([opening], |r| r.get::<_, Value>(0))
        .optional()?
        .map(val_text)
        .unwrap_or_default())
}

fn wall_frame(cx: &mut Ctx, wall: i64, t: f64) -> Result<Option<(V2, V2, V2)>, Error> {
    Ok(cx.wall(wall)?.map(|w| {
        let p = w.at(t, 0.0);
        let n = w.at(t, 1.0) - p;
        let d = w.at((t + 1e-3).min(w.len()), 0.0) - w.at((t - 1e-3).max(0.0), 0.0);
        let unit = |v: V2| v * (1.0 / v.len().max(1e-12));
        (p, unit(n), unit(d))
    }))
}

fn host_info(cx: &mut Ctx, h: TagHost) -> Result<Option<HostInfo>, Error> {
    let db = cx.db;
    Ok(match h {
        TagHost::Opening(id) => {
            let row: Option<(i64, f64, f64, String, i64)> = db
                .prepare_cached(
                    "SELECT o.wall, o.along, o.width, o.kind, w.level FROM opening o
                     JOIN wall w ON w.id=o.wall WHERE o.id=?1",
                )?
                .query_row([id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .optional()?;
            let Some((wall, along, width, kind, level)) = row else {
                return Ok(None);
            };
            let Some((p, n, d)) = wall_frame(cx, wall, along + width / 2.0)? else {
                return Ok(None);
            };
            Some(HostInfo {
                cat: if kind == "door" {
                    TagCat::Door
                } else {
                    TagCat::Window
                },
                level,
                anchor: p,
                normal: n,
                along: d,
                text: TagText {
                    mark: mark_of(db, id)?,
                    ..Default::default()
                },
            })
        }
        TagHost::Wall(id) => {
            let row: Option<(i64, String)> = db
                .prepare_cached(
                    "SELECT w.level, t.name FROM wall w JOIN wall_type t ON t.id=w.wtype WHERE w.id=?1",
                )?
                .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            let Some((level, name)) = row else {
                return Ok(None);
            };
            let len = cx.wall(id)?.map_or(0.0, |w| w.len());
            let Some((p, n, d)) = wall_frame(cx, id, len / 2.0)? else {
                return Ok(None);
            };
            Some(HostInfo {
                cat: TagCat::Wall,
                level,
                anchor: p,
                normal: n,
                along: d,
                text: TagText {
                    mark: name,
                    ..Default::default()
                },
            })
        }
        TagHost::Room(id) => {
            let row: Option<(i64, String, String, f64, f64, Option<f64>)> = db
                .prepare_cached(
                    "SELECT r.level, r.name, r.number, r.px, r.py, q.area FROM room r
                     LEFT JOIN room_qty q ON q.room=r.id WHERE r.id=?1",
                )?
                .query_row([id], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })
                .optional()?;
            row.map(|(level, name, number, x, y, area)| HostInfo {
                cat: TagCat::Room,
                level,
                anchor: v2(x, y),
                normal: v2(0.0, 1.0),
                along: v2(1.0, 0.0),
                text: TagText {
                    mark: number,
                    name,
                    area,
                },
            })
        }
        TagHost::Comp(id) => {
            let row: Option<(i64, String, f64, f64, Option<f64>, Option<f64>)> = db
                .prepare_cached(
                    "SELECT c.level, t.name, c.x, c.y, q.wx, q.wy FROM comp c
                     JOIN comp_type t ON t.id=c.ctype LEFT JOIN comp_qty q ON q.comp=c.id WHERE c.id=?1",
                )?
                .query_row([id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
                })
                .optional()?;
            row.map(|(level, name, x, y, wx, wy)| HostInfo {
                cat: TagCat::Comp,
                level,
                anchor: v2(wx.unwrap_or(x), wy.unwrap_or(y)),
                normal: v2(0.0, 1.0),
                along: v2(1.0, 0.0),
                text: TagText {
                    mark: name,
                    ..Default::default()
                },
            })
        }
    })
}

/// Tag extent in plan (m): by character count, at 1:100 scale.
pub fn tag_size(cat: TagCat, t: &TagText) -> (f64, f64) {
    let chars = |s: &str| s.chars().count().max(1) as f64;
    match cat {
        TagCat::Room => {
            let top = chars(&format!("{} {}", t.mark, t.name));
            (0.16 * top.max(7.0) + 0.3, 0.8)
        }
        _ => (0.16 * chars(&t.mark) + 0.35, 0.42),
    }
}

/// The segment intersects the rectangle [lo, hi] (Liang-Barsky).
fn seg_hits(a: V2, b: V2, lo: V2, hi: V2) -> bool {
    let d = b - a;
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-d.x, a.x - lo.x),
        (d.x, hi.x - a.x),
        (-d.y, a.y - lo.y),
        (d.y, hi.y - a.y),
    ] {
        if p.abs() < 1e-15 {
            if q < 0.0 {
                return false;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

/// Layout obstacles of a level: wall axes with half-thickness.
fn wall_segs(cx: &mut Ctx, level: i64) -> Result<Vec<(V2, V2, f64)>, Error> {
    let ids: Vec<i64> = {
        let mut st = cx.db.prepare("SELECT id FROM wall WHERE level=?1")?;
        let v = st
            .query_map([level], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        v
    };
    let mut out = vec![];
    for id in ids {
        if let Some(w) = cx.wall(id)? {
            let n = if w.arc.is_some() { 8 } else { 1 };
            let l = w.len();
            let half = w.total / 2.0 + (w.offset0 + w.total / 2.0).abs();
            for k in 0..n {
                let (t0, t1) = (l * k as f64 / n as f64, l * (k + 1) as f64 / n as f64);
                out.push((w.at(t0, 0.0), w.at(t1, 0.0), half));
            }
        }
    }
    Ok(out)
}

/// Placement without overlaps: the first candidate clear of placed tags and walls; with none
/// near, a far place with a leader (the third value: a leader is needed).
fn auto_offset(info: &HostInfo, obs: &bins::Obstacles) -> (P, (V2, V2), bool) {
    let (w, h) = tag_size(info.cat, &info.text);
    let (n, d) = (info.normal, info.along);
    let cands: Vec<V2> = match info.cat {
        TagCat::Door | TagCat::Window => {
            let k = 0.2 + h / 2.0 + w.max(h) * 0.25;
            let mut v = vec![];
            for s in [1.0, -1.0] {
                v.push(n * (k * s));
            }
            for s in [1.0, -1.0] {
                for a in [0.9, -0.9] {
                    v.push(n * (k * s) + d * a);
                }
                v.push(n * (2.0 * k * s));
            }
            v
        }
        TagCat::Wall => [1.0, -1.0]
            .into_iter()
            .flat_map(|s| [0.0, 1.2, -1.2].map(|a| n * (0.45 * s) + d * a))
            .collect(),
        TagCat::Room => vec![
            v2(0.0, 0.0),
            v2(0.0, -0.7),
            v2(0.0, 0.7),
            v2(-1.0, 0.0),
            v2(1.0, 0.0),
            v2(0.0, -1.4),
            v2(0.0, 1.4),
        ],
        TagCat::Comp => vec![v2(0.0, 0.6), v2(0.0, -0.6), v2(0.9, 0.0), v2(-0.9, 0.0)],
    };
    let rect = |o: V2| {
        let c = info.anchor + o;
        (c - v2(w / 2.0, h / 2.0), c + v2(w / 2.0, h / 2.0))
    };
    let buf = std::cell::RefCell::new(Vec::new());
    let clear = |o: V2| {
        let (lo, hi) = rect(o);
        obs.clear_of_tags(lo, hi, &mut buf.borrow_mut())
    };
    let free = |o: V2| {
        let (lo, hi) = rect(o);
        clear(o) && obs.clear_of_walls(lo, hi, &mut buf.borrow_mut())
    };
    if let Some(o) = cands.iter().copied().find(|&o| free(o)) {
        return ((o.x, o.y), rect(o), false);
    }
    // no near place: rings further out, with a leader; first clear of walls, then of tags only
    let ring = far_ring(info, w, h);
    let o = ring
        .iter()
        .copied()
        .find(|&o| free(o))
        .or_else(|| ring.iter().copied().find(|&o| clear(o)));
    match o {
        Some(o) => ((o.x, o.y), rect(o), true),
        None => ((cands[0].x, cands[0].y), rect(cands[0]), false),
    }
}

/// Far places for a tag with a leader: rings of 12 directions, nearest first, starting square
/// to the host wall (the room tag: straight up).
fn far_ring(info: &HostInfo, w: f64, h: f64) -> Vec<V2> {
    let (n, d) = match info.cat {
        TagCat::Room | TagCat::Comp => (v2(0.0, 1.0), v2(1.0, 0.0)),
        _ => (info.normal, info.along),
    };
    let step = w.max(h) + 0.2;
    let mut out = vec![];
    for k in 1..=6 {
        let r = step * (1.0 + 0.5 * k as f64);
        for j in 0..12 {
            let a = std::f64::consts::PI * j as f64 / 6.0;
            out.push(n * (r * a.cos()) + d * (r * a.sin()));
        }
    }
    out
}

fn insert(db: &Connection, s: &TagSpec) -> Result<i64, Error> {
    db.execute(
        "INSERT INTO dim(level,kind,refs,dx,dy,side,offset,tier,auto) VALUES (?1,'tag',?2,?3,?4,?5,0,-1,0)",
        params![s.level, s.host.text(), s.offset.0, s.offset.1, if s.leader { 1.0 } else { 0.0 }],
    )?;
    Ok(db.last_insert_rowid())
}

fn tag_rows(db: &Connection, level: Option<i64>) -> Result<Vec<(i64, TagSpec)>, Error> {
    let mut st = db.prepare_cached(
        "SELECT id, level, refs, dx, dy, side FROM dim WHERE kind='tag' AND (?1 IS NULL OR level=?1) ORDER BY id",
    )?;
    let rows = st.query_map([level], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, f64>(3)?,
            r.get::<_, f64>(4)?,
            r.get::<_, f64>(5)?,
        ))
    })?;
    let mut out = vec![];
    for row in rows {
        let (id, level, refs, dx, dy, side) = row?;
        if let Some(host) = TagHost::parse(&refs) {
            out.push((
                id,
                TagSpec {
                    level,
                    host,
                    offset: (dx, dy),
                    leader: side != 0.0,
                },
            ));
        }
    }
    Ok(out)
}

fn exists(db: &Connection, h: TagHost) -> Result<bool, Error> {
    Ok(db
        .prepare_cached(&format!("SELECT 1 FROM {} WHERE id=?1", h.table()))?
        .exists([h.id()])?)
}

/// Tags of deleted hosts and numbers of deleted openings are removed (undo restores them).
pub(crate) fn cascade(db: &Connection) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    for (id, s) in tag_rows(db, None)? {
        if !exists(db, s.host)? {
            inv = inv.restore(db, "dim", "id", id)?;
            db.execute("DELETE FROM dim WHERE id=?1", [id])?;
        }
    }
    let gone: Vec<(i64, i64, Value, String)> = {
        let mut st = db.prepare_cached(
            "SELECT rowid, ref, value, unit FROM prop p WHERE kind='opening' AND key='mark'
             AND NOT EXISTS (SELECT 1 FROM opening o WHERE o.id=p.ref)",
        )?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    // rowid is preserved: otherwise undoing a batch operation with a prop would delete the restored number
    for (row, r, v, u) in gone {
        inv = inv.sql(
            "INSERT OR REPLACE INTO prop(rowid,kind,ref,key,value,unit) VALUES (?1,'opening',?2,'mark',?3,?4)",
            vec![Value::Integer(row), Value::Integer(r), v, Value::Text(u)],
        );
        db.execute(
            "DELETE FROM prop WHERE kind='opening' AND ref=?1 AND key='mark'",
            [r],
        )?;
    }
    Ok(inv)
}

/// Integrity: a tag without a host, a number without an opening.
pub(crate) fn issues(db: &Connection) -> Result<Vec<crate::integrity::Issue>, Error> {
    let mut out = vec![];
    for (id, s) in tag_rows(db, None)? {
        if !exists(db, s.host)? {
            out.push(crate::integrity::Issue {
                table: "dim".into(),
                id,
                problem: format!("tag of missing {} {}", s.host.table(), s.host.id()),
            });
        }
    }
    let mut st = db.prepare(
        "SELECT ref FROM prop p WHERE kind='opening' AND key='mark'
         AND NOT EXISTS (SELECT 1 FROM opening o WHERE o.id=p.ref)",
    )?;
    for r in st.query_map([], |r| r.get::<_, i64>(0))? {
        out.push(crate::integrity::Issue {
            table: "prop".into(),
            id: r?,
            problem: "mark of missing opening".into(),
        });
    }
    Ok(out)
}

/// Next number in a series: the last digit group plus 1, preserving width ("009" -> "010",
/// "D9" -> "D10"); a letter after the digits advances to the next one ("101a" -> "101b"); otherwise "...1" is appended.
pub fn next_mark(s: &str) -> String {
    let cs: Vec<char> = s.chars().collect();
    let digits = cs.iter().rev().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let head: String = cs[..cs.len() - digits].iter().collect();
        let tail: String = cs[cs.len() - digits..].iter().collect();
        let n: u128 = tail.parse().unwrap_or(0) + 1;
        return format!("{head}{n:0digits$}");
    }
    match cs.last() {
        Some(&c) if c.is_ascii_alphabetic() && c != 'z' && c != 'Z' => {
            let mut v = cs.clone();
            *v.last_mut().unwrap() = (c as u8 + 1) as char;
            v.into_iter().collect()
        }
        _ => format!("{s}1"),
    }
}

/// Level hosts of categories `cats`.
fn hosts(db: &Connection, level: i64, cats: &[TagCat]) -> Result<Vec<TagHost>, Error> {
    let ids = |sql: &str| -> Result<Vec<i64>, Error> {
        let mut st = db.prepare(sql)?;
        let v = st
            .query_map([level], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    };
    let mut out = vec![];
    for c in cats {
        match c {
            TagCat::Door => out.extend(
                ids("SELECT o.id FROM opening o JOIN wall w ON w.id=o.wall WHERE w.level=?1 AND o.kind='door' ORDER BY o.id")?
                    .into_iter()
                    .map(TagHost::Opening),
            ),
            TagCat::Window => out.extend(
                ids("SELECT o.id FROM opening o JOIN wall w ON w.id=o.wall WHERE w.level=?1 AND o.kind='window' ORDER BY o.id")?
                    .into_iter()
                    .map(TagHost::Opening),
            ),
            TagCat::Room => out.extend(
                ids("SELECT id FROM room WHERE level=?1 ORDER BY id")?
                    .into_iter()
                    .map(TagHost::Room),
            ),
            TagCat::Wall => out.extend(
                ids("SELECT id FROM wall WHERE level=?1 ORDER BY id")?
                    .into_iter()
                    .map(TagHost::Wall),
            ),
            TagCat::Comp => out.extend(
                ids("SELECT id FROM comp WHERE level=?1 ORDER BY id")?
                    .into_iter()
                    .map(TagHost::Comp),
            ),
        }
    }
    Ok(out)
}

/// Layout of new level tags on top of existing ones; None for a host means it does not exist.
fn plan_tags(
    db: &Connection,
    level: i64,
    new: &[TagHost],
    leader: bool,
) -> Result<Vec<TagSpec>, Error> {
    let mut cx = Ctx {
        db,
        walls: BTreeMap::new(),
    };
    let mut obs = bins::Obstacles::new(wall_segs(&mut cx, level)?);
    for (_, s) in tag_rows(db, Some(level))? {
        if let Some(i) = host_info(&mut cx, s.host)? {
            let (w, h) = tag_size(i.cat, &i.text);
            let c = i.anchor + v2(s.offset.0, s.offset.1);
            obs.add_box((c - v2(w / 2.0, h / 2.0), c + v2(w / 2.0, h / 2.0)));
        }
    }
    let mut out = vec![];
    for &h in new {
        let info = host_info(&mut cx, h)?
            .ok_or_else(|| Error::Invalid(format!("tag: no such {} {}", h.table(), h.id())))?;
        let (off, b, far) = auto_offset(&info, &obs);
        obs.add_box(b);
        out.push(TagSpec {
            level: info.level,
            host: h,
            offset: off,
            leader: leader || far,
        });
    }
    Ok(out)
}

/// Exterior face of a straight wall: a ray outward from the face midpoint hits no other walls.
pub(super) fn exterior_face(cx: &mut Ctx, level: i64, id: i64) -> Result<u8, Error> {
    let ids: Vec<i64> = {
        let mut st = cx.db.prepare("SELECT id FROM wall WHERE level=?1")?;
        let v = st
            .query_map([level], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        v
    };
    let mut feet = vec![];
    for &o in ids.iter().filter(|&&o| o != id) {
        if let Some(w) = cx.wall(o)? {
            let (o0, o1, l) = (w.face(0), w.face(1), w.len());
            let n = if w.arc.is_some() { 16 } else { 1 };
            let mut ring: Vec<V2> = (0..=n).map(|k| w.at(l * k as f64 / n as f64, o0)).collect();
            ring.extend((0..=n).rev().map(|k| w.at(l * k as f64 / n as f64, o1)));
            feet.push(ring);
        }
    }
    let Some(w) = cx.wall(id)? else {
        return Ok(1);
    };
    let l = w.len();
    for face in [1u8, 0] {
        let (m, oth) = (w.at(l / 2.0, w.face(face)), w.at(l / 2.0, w.face(1 - face)));
        let d = m - oth;
        let n = d * (1.0 / d.len().max(1e-12));
        if feet.iter().all(|f| !dim::ray_hits(m + n * 1e-4, n, f, 0.0)) {
            return Ok(face);
        }
    }
    Ok(1)
}

impl Model {
    /// Tags in one operation (one undo step).
    pub fn add_tags(&mut self, specs: &[TagSpec]) -> Result<Vec<i64>, Error> {
        for s in specs {
            if !(s.offset.0.is_finite() && s.offset.1.is_finite()) {
                return Err(Error::Invalid("tag offset must be finite".into()));
            }
            if !exists(&self.db, s.host)? {
                return Err(Error::Invalid(format!(
                    "tag: no such {} {}",
                    s.host.table(),
                    s.host.id()
                )));
            }
        }
        self.op("tag_add", |db| {
            let mut inv = Inv::default();
            let mut ids = vec![];
            for s in specs {
                let id = insert(db, s)?;
                inv = inv.delete("dim", "id", id);
                ids.push(id);
            }
            Ok((ids.clone(), ids.first().copied().unwrap_or(0), inv))
        })
    }

    /// Tag an element on click: the position is the first free spot next to the host.
    pub fn tag_element(&mut self, level: i64, host: TagHost, leader: bool) -> Result<i64, Error> {
        let specs = plan_tags(&self.db, level, &[host], leader)?;
        Ok(self.add_tags(&specs)?[0])
    }

    /// Hosts of categories `cats` on the level that have no tag.
    pub fn untagged(&self, level: i64, cats: &[TagCat]) -> Result<Vec<TagHost>, Error> {
        let have: BTreeSet<TagHost> = tag_rows(&self.db, None)?
            .into_iter()
            .map(|(_, s)| s.host)
            .collect();
        Ok(hosts(&self.db, level, cats)?
            .into_iter()
            .filter(|h| !have.contains(h))
            .collect())
    }

    /// "Tag all": every untagged host of the categories, without overlaps, in one operation.
    pub fn tag_all(
        &mut self,
        level: i64,
        cats: &[TagCat],
        leader: bool,
    ) -> Result<Vec<i64>, Error> {
        let new = self.untagged(level, cats)?;
        if new.is_empty() {
            return Ok(vec![]);
        }
        let specs = plan_tags(&self.db, level, &new, leader)?;
        self.add_tags(&specs)
    }

    /// Head offsets and leaders of the tags of `specs`' hosts, one operation; a host with no
    /// tag on its level is skipped. The count of tags moved.
    pub fn place_tags(&mut self, specs: &[TagSpec]) -> Result<usize, Error> {
        if specs
            .iter()
            .any(|s| !(s.offset.0.is_finite() && s.offset.1.is_finite()))
        {
            return Err(Error::Invalid("tag offset must be finite".into()));
        }
        let mut have: BTreeMap<(i64, TagHost), (i64, TagSpec)> = BTreeMap::new();
        let levels: BTreeSet<i64> = specs.iter().map(|s| s.level).collect();
        for lv in levels {
            for (id, t) in tag_rows(&self.db, Some(lv))? {
                have.insert((lv, t.host), (id, t));
            }
        }
        let mut moves = vec![];
        for s in specs {
            if let Some((id, t)) = have.get(&(s.level, s.host)) {
                if t.offset != s.offset || t.leader != s.leader {
                    moves.push((*id, *s));
                }
            }
        }
        if moves.is_empty() {
            return Ok(0);
        }
        self.op("tag_place", |db| {
            let mut inv = Inv::default();
            for (id, s) in &moves {
                inv = inv.restore(db, "dim", "id", *id)?;
                db.execute(
                    "UPDATE dim SET dx=?2, dy=?3, side=?4 WHERE id=?1 AND kind='tag'",
                    params![id, s.offset.0, s.offset.1, if s.leader { 1.0 } else { 0.0 }],
                )?;
            }
            Ok((moves.len(), moves[0].0, inv))
        })
    }

    /// New head offset (a manual shift is kept: the host moves the tag as a whole).
    pub fn move_tag(&mut self, id: i64, offset: P) -> Result<(), Error> {
        if !(offset.0.is_finite() && offset.1.is_finite()) {
            return Err(Error::Invalid("tag offset must be finite".into()));
        }
        self.op("tag_move", |db| {
            let inv = Inv::default().restore(db, "dim", "id", id)?;
            let n = db.execute(
                "UPDATE dim SET dx=?2, dy=?3 WHERE id=?1 AND kind='tag'",
                params![id, offset.0, offset.1],
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no such tag {id}")));
            }
            Ok(((), id, inv))
        })
    }

    pub fn set_tag_leader(&mut self, id: i64, leader: bool) -> Result<(), Error> {
        self.op("tag_leader", |db| {
            let inv = Inv::default().restore(db, "dim", "id", id)?;
            let n = db.execute(
                "UPDATE dim SET side=?2 WHERE id=?1 AND kind='tag'",
                params![id, if leader { 1.0 } else { 0.0 }],
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no such tag {id}")));
            }
            Ok(((), id, inv))
        })
    }

    pub fn delete_tag(&mut self, id: i64) -> Result<(), Error> {
        self.op("tag_delete", |db| {
            let inv = Inv::default().restore(db, "dim", "id", id)?;
            if db.execute("DELETE FROM dim WHERE id=?1 AND kind='tag'", [id])? == 0 {
                return Err(Error::Invalid(format!("no such tag {id}")));
            }
            Ok(((), id, inv))
        })
    }

    /// Level tags with the hosts' current positions; tags of vanished hosts are skipped.
    pub fn tags(&self, level: i64) -> Result<Vec<Tag>, Error> {
        let mut cx = Ctx {
            db: &self.db,
            walls: BTreeMap::new(),
        };
        let mut out = vec![];
        for (id, spec) in tag_rows(&self.db, Some(level))? {
            // a host moved to another level does not drag along a tag belonging to another view
            if let Some(i) = host_info(&mut cx, spec.host)?.filter(|i| i.level == level) {
                let head = i.anchor + v2(spec.offset.0, spec.offset.1);
                out.push(Tag {
                    id,
                    spec,
                    cat: i.cat,
                    anchor: (i.anchor.x, i.anchor.y),
                    head: (head.x, head.y),
                    text: i.text,
                });
            }
        }
        Ok(out)
    }

    /// Host number/mark: an opening uses `prop mark`, a room uses its number.
    pub fn mark(&self, host: TagHost) -> Result<String, Error> {
        match host {
            TagHost::Opening(id) => mark_of(&self.db, id),
            TagHost::Room(id) => Ok(self
                .db
                .query_row("SELECT number FROM room WHERE id=?1", [id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| Error::Invalid(format!("no such room {id}")))?),
            _ => Err(Error::Invalid(
                "only doors, windows and rooms are numbered".into(),
            )),
        }
    }

    /// Set the number (one operation). An empty string clears the opening's number.
    pub fn set_mark(&mut self, host: TagHost, text: &str) -> Result<(), Error> {
        let text = text.trim().to_string();
        match host {
            TagHost::Opening(id) => self.op("tag_mark", |db| {
                let n: i64 =
                    db.query_row("SELECT count(*) FROM opening WHERE id=?1", [id], |r| r.get(0))?;
                if n == 0 {
                    return Err(Error::Invalid(format!("no such opening {id}")));
                }
                let old: Option<(Value, String)> = db
                    .query_row(
                        "SELECT value, unit FROM prop WHERE kind='opening' AND ref=?1 AND key='mark'",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let inv = match old {
                    Some((v, u)) => Inv::default().sql(
                        "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES ('opening',?1,'mark',?2,?3)",
                        vec![Value::Integer(id), v, Value::Text(u)],
                    ),
                    None => Inv::default().sql(
                        "DELETE FROM prop WHERE kind='opening' AND ref=?1 AND key='mark'",
                        vec![Value::Integer(id)],
                    ),
                };
                if text.is_empty() {
                    db.execute(
                        "DELETE FROM prop WHERE kind='opening' AND ref=?1 AND key='mark'",
                        [id],
                    )?;
                } else {
                    db.execute(
                        "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES ('opening',?1,'mark',?2,'')",
                        params![id, text],
                    )?;
                }
                Ok(((), id, inv))
            }),
            TagHost::Room(id) => self.op("tag_mark", |db| {
                let inv = Inv::default().restore(db, "room", "id", id)?;
                if db.execute("UPDATE room SET number=?2 WHERE id=?1", params![id, text])? == 0 {
                    return Err(Error::Invalid(format!("no such room {id}")));
                }
                Ok(((), id, inv))
            }),
            _ => Err(Error::Invalid("only doors, windows and rooms are numbered".into())),
        }
    }

    /// Inline tag edit: number/mark (and the room name when `name` is given) in one operation.
    /// A pinned host is refused; an empty mark clears it.
    pub fn set_tag_text(
        &mut self,
        host: TagHost,
        mark: &str,
        name: Option<&str>,
    ) -> Result<(), Error> {
        let (kind, id) = match host {
            TagHost::Opening(id) => ("opening", id),
            TagHost::Room(id) => ("room", id),
            _ => return Err(Error::Invalid("only doors, windows and rooms are numbered".into())),
        };
        if self.is_pinned(kind, id)? {
            return Err(Error::Invalid("element is pinned".into()));
        }
        match (host, name) {
            (TagHost::Room(_), Some(name)) => {
                let (mark, name) = (mark.trim().to_string(), name.trim().to_string());
                self.op("tag_mark", |db| {
                    let inv = Inv::default().restore(db, "room", "id", id)?;
                    let n = db.execute(
                        "UPDATE room SET number=?2, name=?3 WHERE id=?1",
                        params![id, mark, name],
                    )?;
                    if n == 0 {
                        return Err(Error::Invalid(format!("no such room {id}")));
                    }
                    Ok(((), id, inv))
                })
            }
            _ => self.set_mark(host, mark),
        }
    }

    /// How many other hosts of the same group (doors, windows or rooms) already carry `mark`.
    pub fn other_marks(&self, host: TagHost, mark: &str) -> Result<i64, Error> {
        let mark = mark.trim();
        if mark.is_empty() {
            return Ok(0);
        }
        Ok(match host {
            TagHost::Opening(id) => self.db.query_row(
                "SELECT count(*) FROM prop p JOIN opening o ON o.id=p.ref                  WHERE p.kind='opening' AND p.key='mark' AND CAST(p.value AS TEXT)=?1 AND p.ref<>?2                  AND o.kind=(SELECT kind FROM opening WHERE id=?2)",
                params![mark, id],
                |r| r.get(0),
            )?,
            TagHost::Room(id) => self.db.query_row(
                "SELECT count(*) FROM room WHERE trim(number)=?1 AND id<>?2",
                params![mark, id],
                |r| r.get(0),
            )?,
            _ => 0,
        })
    }

    /// Chains for the selected straight walls on the exterior face: openings, grid axes, overall size.
    /// A repeat does not duplicate: identical manual dimensions are replaced. One operation.
    pub fn auto_dimension_walls(&mut self, level: i64, walls: &[i64]) -> Result<Vec<i64>, Error> {
        let specs = wall_specs(&self.db, level, walls)?;
        if specs.is_empty() {
            return Err(Error::Invalid("no straight walls to dimension".into()));
        }
        self.op("dim_auto_walls", |db| {
            let (ids, inv) = replace_linear(db, level, &specs)?;
            Ok((ids, level, inv))
        })
    }
}

/// New manual dimensions replacing identical old ones.
pub(super) fn replace_linear(
    db: &Connection,
    level: i64,
    specs: &[DimSpec],
) -> Result<(Vec<i64>, Inv), Error> {
    let mut inv = Inv::default();
    let old: Vec<(i64, String, String)> = {
        let mut st =
            db.prepare("SELECT id, kind, refs FROM dim WHERE level=?1 AND auto=0 AND kind<>'tag'")?;
        let v = st
            .query_map([level], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    let mut ids = vec![];
    let mut seen = BTreeSet::new();
    for s in specs {
        let refs = refs_text(&s.refs);
        // a wall repeated in the selection does not produce double chains
        if !seen.insert(refs.clone()) {
            continue;
        }
        for (id, k, r) in &old {
            if *r == refs && k == "linear" {
                inv = inv.restore(db, "dim", "id", *id)?;
                db.execute("DELETE FROM dim WHERE id=?1", [id])?;
            }
        }
        let id = super::insert(db, s)?;
        inv = Inv::default().delete("dim", "id", id).then(inv);
        ids.push(id);
    }
    Ok((ids, inv))
}

fn wall_specs(db: &Connection, level: i64, walls: &[i64]) -> Result<Vec<DimSpec>, Error> {
    let mut cx = Ctx {
        db,
        walls: BTreeMap::new(),
    };
    let grids: Vec<i64> = {
        let mut st = db.prepare("SELECT id FROM grid WHERE bulge=0 ORDER BY id")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        v
    };
    let mut out = vec![];
    for &wid in walls {
        let Some(w) = cx.wall(wid)? else { continue };
        if w.arc.is_some() || w.len() < 1e-6 {
            continue;
        }
        let (a, b) = (v2(w.a.0, w.a.1), v2(w.b.0, w.b.1));
        let dir = (b - a) * (1.0 / (b - a).len());
        let face = exterior_face(&mut cx, level, wid)?;
        let w = cx.wall(wid)?.expect("wall cached");
        // outward is left of the axis when this face is left of the opposite one
        let side = if w.face(face) >= w.face(1 - face) {
            1.0
        } else {
            -1.0
        };
        let base = DimRef::WallEnd {
            wall: wid,
            face,
            end: 0,
        };
        let ends = [
            base,
            DimRef::WallEnd {
                wall: wid,
                face,
                end: 1,
            },
        ];
        let mut tier = 0;
        let mut chain =
            |refs: Vec<DimRef>, cx: &mut Ctx, out: &mut Vec<DimSpec>| -> Result<bool, Error> {
                let mut pts = vec![];
                for r in &refs {
                    if let Some(p) = cx.point(r)? {
                        pts.push((*r, p.2));
                    }
                }
                let vs: Vec<V2> = pts.iter().map(|p| p.1).collect();
                let pairs = dim::chain(&vs, dir, 1e-6);
                if pairs.is_empty() {
                    return Ok(false);
                }
                for (i, j) in pairs {
                    out.push(DimSpec {
                        level,
                        kind: DimKind::Linear,
                        refs: vec![pts[i].0, pts[j].0, base],
                        dir: (dir.x, dir.y),
                        side,
                        offset: 0.7 + 0.6 * tier as f64,
                        tier: -1,
                        auto: false,
                    });
                }
                tier += 1;
                Ok(true)
            };
        let ops: Vec<i64> = {
            let mut st = db.prepare("SELECT id FROM opening WHERE wall=?1 ORDER BY along")?;
            let v = st
                .query_map([wid], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            v
        };
        if !ops.is_empty() {
            let mut refs = ends.to_vec();
            for o in &ops {
                for edge in [0u8, 1] {
                    refs.push(DimRef::OpeningEdge {
                        opening: *o,
                        edge,
                        face,
                    });
                }
            }
            chain(refs, &mut cx, &mut out)?;
        }
        let len = cx.wall(wid)?.map_or(0.0, |w| w.len());
        let mut grefs = vec![];
        for &g in &grids {
            let r = DimRef::GridAt { grid: g, wall: wid };
            if let Some((_, t, _)) = cx.point(&r)? {
                if t > -1e-6 && t < len + 1e-6 {
                    grefs.push(r);
                }
            }
        }
        if !grefs.is_empty() {
            let mut refs = ends.to_vec();
            refs.extend(grefs);
            chain(refs, &mut cx, &mut out)?;
        }
        chain(ends.to_vec(), &mut cx, &mut out)?;
    }
    Ok(out)
}

#[path = "annot_tags_bins.rs"]
mod bins;

#[cfg(test)]
#[path = "annot_tags_tests.rs"]
mod tests;
