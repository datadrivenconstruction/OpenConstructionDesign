//! Rooms: an interior point + name/number/height. The outline is a face of the arrangement of the
//! level's wall footprints (`ddc_geom::rooms::arrange`). Decision: recompute immediately on any
//! wall/room edit in the same transaction (like `wall_qty`), with no "stale" flag; no `room_qty`
//! row means the room is not enclosed.

use ddc_geom::rooms::{arrange_with_lines, Arrangement, Region};
use ddc_geom::{v2, Tol, V2};
use rusqlite::{params, Connection};

use crate::ops::Inv;
use crate::slabs::profile;
use crate::walls::{level_walls, room_wall_polys, LevelPolys};
use crate::{Error, Model, SlabSpec};

#[derive(Clone, Debug, PartialEq)]
pub struct RoomSpec {
    pub level: i64,
    pub name: String,
    pub number: String,
    /// A point inside the room.
    pub point: (f64, f64),
    /// Height from the level to the ceiling: volume = area × height.
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoomQty {
    pub area: f64,
    pub perimeter: f64,
    pub volume: f64,
    /// [outer (counter-clockwise), holes (clockwise)...].
    pub rings: Vec<Vec<(f64, f64)>>,
}

/// Room separator: a segment on a level.
#[derive(Clone, Debug, PartialEq)]
pub struct SepSpec {
    pub level: i64,
    pub a: (f64, f64),
    pub b: (f64, f64),
}

/// What bounds a room: a wall or a separator (id).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Boundary {
    Wall(i64),
    Sep(i64),
}

fn invalid<T>(msg: String) -> Result<T, Error> {
    Err(Error::Invalid(msg))
}

pub(crate) fn check(db: &Connection, r: &RoomSpec) -> Result<(), Error> {
    let lv: i64 = db.query_row("SELECT count(*) FROM level WHERE id=?1", [r.level], |x| {
        x.get(0)
    })?;
    let ok = lv > 0
        && r.height.is_finite()
        && r.height > 0.0
        && r.point.0.is_finite()
        && r.point.1.is_finite();
    if !ok {
        return invalid(format!("room: bad level {}, height or point", r.level));
    }
    Ok(())
}

fn region_rings(g: &Region) -> Vec<Vec<(f64, f64)>> {
    std::iter::once(&g.outer)
        .chain(&g.holes)
        .map(|r| r.iter().map(|p| (p.x, p.y)).collect())
        .collect()
}

type Bbox = [f64; 4];

fn bbox(pts: impl Iterator<Item = V2>, pad: f64) -> Bbox {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in pts {
        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
    }
    [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad]
}

fn overlap(a: &Bbox, b: &Bbox) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

fn find(p: &mut [usize], mut i: usize) -> usize {
    while p[i] != i {
        p[i] = p[p[i]];
        i = p[i];
    }
    i
}

/// Components by bounding-box overlap (sweep along x): unconnected wall groups do not affect each
/// other's faces, so a room only needs the groups whose boxes touch the ones enclosing it.
fn clusters(b: &[Bbox]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..b.len()).collect();
    order.sort_by(|&i, &j| b[i][0].total_cmp(&b[j][0]));
    let mut parent: Vec<usize> = (0..b.len()).collect();
    let mut active: Vec<usize> = vec![];
    for &i in &order {
        active.retain(|&j| b[j][2] >= b[i][0]);
        for &j in &active {
            if overlap(&b[i], &b[j]) {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri.max(rj)] = ri.min(rj);
                }
            }
        }
        active.push(i);
    }
    (0..b.len()).map(|i| find(&mut parent, i)).collect()
}

/// Recomputes `room_qty` from the wall footprints of each level; `scoped` limits it to rooms of
/// the listed levels (rows of other levels are not touched).
pub(crate) fn regenerate(
    db: &Connection,
    levels: &[LevelPolys],
    scoped: bool,
) -> Result<(), Error> {
    if !scoped {
        db.execute("DELETE FROM room_qty", [])?;
    }
    let pad = 4.0 * Tol::default().length;
    for (level, elev, base_polys, faces) in levels {
        // leaning walls bound rooms by their section at the room cut height
        let cut = crate::walls::leaning_section(db, *level, *elev)?;
        let polys = cut.as_ref().unwrap_or(base_polys);
        if scoped {
            db.execute(
                "DELETE FROM room_qty WHERE room IN (SELECT id FROM room WHERE level=?1)",
                [level],
            )?;
        }
        let rooms: Vec<(i64, f64, f64, f64)> = {
            let mut st =
                db.prepare("SELECT id, px, py, height FROM room WHERE level=?1 ORDER BY id")?;
            let rows = st.query_map([level], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            rows.collect::<Result<_, _>>()?
        };
        let mut open_rooms = vec![];
        if rooms.is_empty() {
            continue;
        }
        let lines = level_lines(db, *level)?;
        let boxes: Vec<Bbox> = polys
            .iter()
            .map(|r| bbox(r.iter().copied(), pad))
            .chain(lines.iter().map(|l| bbox([l.0, l.1].into_iter(), pad)))
            .collect();
        let comp = clusters(&boxes);
        let mut cbox: std::collections::BTreeMap<usize, Bbox> = Default::default();
        for (i, c) in comp.iter().enumerate() {
            let e = cbox.entry(*c).or_insert(boxes[i]);
            *e = [
                e[0].min(boxes[i][0]),
                e[1].min(boxes[i][1]),
                e[2].max(boxes[i][2]),
                e[3].max(boxes[i][3]),
            ];
        }
        let mut cache: std::collections::HashMap<Vec<usize>, Arrangement> = Default::default();
        // one region, one count: the lowest room id keeps the quantities (rooms come by id)
        let mut held: std::collections::HashSet<(Vec<usize>, usize)> = Default::default();
        let mut held_rings: std::collections::HashSet<String> = Default::default();
        for (id, x, y, h) in rooms {
            let pt = [x, y, x, y];
            let around: Vec<&Bbox> = cbox.values().filter(|b| overlap(b, &pt)).collect();
            if around.is_empty() {
                open_rooms.push((id, h));
                continue;
            }
            let reach = around.iter().fold(*around[0], |a, b| {
                [
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[2].max(b[2]),
                    a[3].max(b[3]),
                ]
            });
            let key: Vec<usize> = cbox
                .iter()
                .filter(|(_, b)| overlap(b, &reach))
                .map(|(c, _)| *c)
                .collect();
            let a = cache.entry(key.clone()).or_insert_with_key(|key| {
                let n = polys.len();
                let key: std::collections::HashSet<usize> = key.iter().copied().collect();
                let ps: Vec<Vec<V2>> = (0..n)
                    .filter(|&i| key.contains(&comp[i]))
                    .map(|i| polys[i].clone())
                    .collect();
                let ls: Vec<(V2, V2)> = (0..lines.len())
                    .filter(|&i| key.contains(&comp[n + i]))
                    .map(|i| lines[i])
                    .collect();
                arrange_with_lines(&ps, &ls, Tol::default())
            });
            let Some(gi) = a.rooms.iter().position(|g| g.contains(v2(x, y))) else {
                open_rooms.push((id, h));
                continue;
            };
            let g = &a.rooms[gi];
            let rings = serde_json::to_string(&region_rings(g))?;
            let new_region = held.insert((key, gi));
            if !(held_rings.insert(rings.clone()) && new_region) {
                // a duplicate: same outline, zero quantities (see `room_duplicates`)
                db.execute(
                    "INSERT INTO room_qty(room, area, perimeter, volume, rings) VALUES (?1,0,0,0,?2)",
                    params![id, rings],
                )?;
                continue;
            }
            let (da, dp) = ddc_geom::arc::arc_correction(g, faces);
            let (area, perimeter) = (g.area + da, g.perimeter + dp);
            db.execute(
                "INSERT INTO room_qty(room, area, perimeter, volume, rings) VALUES (?1,?2,?3,?4,?5)",
                params![id, area, perimeter, area * h, rings],
            )?;
        }
        for (id, h) in open_rooms {
            outline_qty(db, id, h)?;
        }
    }
    Ok(())
}

/// Room not closed by walls: quantities from its imported outline, if any.
fn outline_qty(db: &Connection, id: i64, h: f64) -> Result<(), Error> {
    let s: Option<String> =
        db.query_row("SELECT outline FROM room WHERE id=?1", [id], |r| r.get(0))?;
    let Some(ring) = s.and_then(|s| serde_json::from_str::<Vec<(f64, f64)>>(&s).ok()) else {
        return Ok(());
    };
    let n = ring.len();
    if n < 3 || ring.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        return Ok(());
    }
    let (mut a2, mut per) = (0.0, 0.0);
    for i in 0..n {
        let (p, q) = (ring[i], ring[(i + 1) % n]);
        a2 += p.0 * q.1 - q.0 * p.1;
        per += (q.0 - p.0).hypot(q.1 - p.1);
    }
    let area = 0.5 * a2.abs();
    if area <= Tol::default().length.powi(2) {
        return Ok(());
    }
    db.execute(
        "INSERT INTO room_qty(room, area, perimeter, volume, rings) VALUES (?1,?2,?3,?4,?5)",
        params![id, area, per, area * h, serde_json::to_string(&vec![ring])?],
    )?;
    Ok(())
}

/// A plan segment.
pub type Seg = ((f64, f64), (f64, f64));

/// Tag of the first room (`rooms`: point, tag) that sits in the closed region holding `p`;
/// `None` when `p` is outside every region or the region holds no room yet.
pub fn region_holder<T: Clone>(
    regions: &[Region],
    rooms: &[((f64, f64), T)],
    p: (f64, f64),
) -> Option<T> {
    let region = regions.iter().find(|r| r.contains(v2(p.0, p.1)))?;
    rooms
        .iter()
        .find(|(q, _)| region.contains(v2(q.0, q.1)))
        .map(|(_, t)| t.clone())
}

/// For level rooms (point, outer outline) not enclosed by walls and separators: the outline edges
/// outside wall footprints (farther than 5 cm) that are suitable as separators; `None` means the room is enclosed.
pub fn room_closers(
    db: &Connection,
    level: i64,
    rooms: &[((f64, f64), Vec<(f64, f64)>)],
) -> Result<Vec<Option<Vec<Seg>>>, Error> {
    const NEAR: f64 = 0.05;
    let elev: f64 = db
        .query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
            r.get(0)
        })
        .map_err(|_| Error::Invalid(format!("no such level {level}")))?;
    let polys = room_wall_polys(db, level, elev)?;
    let a = arrange_with_lines(&polys, &level_lines(db, level)?, Tol::default());
    let boxes: Vec<Bbox> = polys
        .iter()
        .map(|r| bbox(r.iter().copied(), NEAR))
        .collect();
    let near = |m: V2| {
        polys.iter().zip(&boxes).any(|(p, b)| {
            overlap(b, &[m.x, m.y, m.x, m.y])
                && (ddc_geom::predicates::point_in_ring(m, p) >= 0
                    || (0..p.len()).any(|i| seg_dist(m, p[i], p[(i + 1) % p.len()]) < NEAR))
        })
    };
    Ok(rooms
        .iter()
        .map(|((x, y), ring)| {
            if a.rooms.iter().any(|g| g.contains(v2(*x, *y))) {
                return None;
            }
            let n = ring.len();
            Some(
                (0..n)
                    .map(|i| (ring[i], ring[(i + 1) % n]))
                    .filter(|(p, q)| {
                        let (p, q) = (v2(p.0, p.1), v2(q.0, q.1));
                        (q - p).len() > 1e-3 && !near((p + q) * 0.5)
                    })
                    .collect(),
            )
        })
        .collect())
}

fn seg_dist(m: V2, a: V2, b: V2) -> f64 {
    let d = b - a;
    let l2 = d.dot(d);
    let t = if l2 > 0.0 {
        ((m - a).dot(d) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (m - (a + d * t)).len()
}

/// Room separators and curtain-wall axes of a level (an arc is 16 chords).
fn level_lines(db: &Connection, level: i64) -> Result<Vec<(V2, V2)>, Error> {
    let mut st = db.prepare("SELECT ax,ay,bx,by FROM room_sep WHERE level=?1 ORDER BY id")?;
    let rows = st.query_map([level], |r| {
        Ok((v2(r.get(0)?, r.get(1)?), v2(r.get(2)?, r.get(3)?)))
    })?;
    let mut out: Vec<(V2, V2)> = rows.collect::<Result<_, _>>()?;
    out.extend(curtain_lines(db, level)?);
    Ok(out)
}

/// Curtain-wall axes of a level (an arc is 16 chords).
fn curtain_lines(db: &Connection, level: i64) -> Result<Vec<(V2, V2)>, Error> {
    let mut out: Vec<(V2, V2)> = vec![];
    let mut st = db.prepare("SELECT id FROM curtain_wall WHERE level=?1 ORDER BY id")?;
    let ids: Vec<i64> = st
        .query_map([level], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for id in ids {
        let Ok(ax) = crate::curtain::read(db, id).and_then(|s| crate::curtain::axis(&s)) else {
            continue;
        };
        let n = if ax.arc.is_some() { 16 } else { 1 };
        let len = ax.length();
        out.extend((0..n).map(|k| {
            (
                ax.point_at(len * k as f64 / n as f64),
                ax.point_at(len * (k + 1) as f64 / n as f64),
            )
        }));
    }
    Ok(out)
}

fn check_sep(db: &Connection, s: &SepSpec) -> Result<(), Error> {
    let lv: i64 = db.query_row("SELECT count(*) FROM level WHERE id=?1", [s.level], |x| {
        x.get(0)
    })?;
    let fin = [s.a.0, s.a.1, s.b.0, s.b.1].iter().all(|v| v.is_finite());
    if lv == 0 || !fin || (s.a.0 - s.b.0).hypot(s.a.1 - s.b.1) <= Tol::default().length {
        return invalid(format!(
            "room separator: bad level {} or degenerate/non-finite segment",
            s.level
        ));
    }
    Ok(())
}

/// Like `arrangement`, but the boundaries are only walls and curtain walls: a separator does not split a slab.
fn wall_arrangement(db: &Connection, level: i64) -> Result<Arrangement, Error> {
    let elev: f64 = db
        .query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
            r.get(0)
        })
        .map_err(|_| Error::Invalid(format!("no such level {level}")))?;
    Ok(arrange_with_lines(
        &room_wall_polys(db, level, elev)?,
        &curtain_lines(db, level)?,
        Tol::default(),
    ))
}

fn arrangement(db: &Connection, level: i64) -> Result<Arrangement, Error> {
    let elev: f64 = db
        .query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
            r.get(0)
        })
        .map_err(|_| Error::Invalid(format!("no such level {level}")))?;
    Ok(arrange_with_lines(
        &room_wall_polys(db, level, elev)?,
        &level_lines(db, level)?,
        Tol::default(),
    ))
}

fn to_spec(level: i64, slab_type: i64, offset: f64, outer: &[V2], holes: &[Vec<V2>]) -> SlabSpec {
    let ring = |r: &[V2]| r.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
    SlabSpec {
        level,
        slab_type,
        offset,
        outer: ring(outer),
        holes: holes.iter().map(|h| ring(h)).collect(),
    }
}

/// A copied room takes the next free number of its level; the original keeps its own.
pub(crate) fn renumber_copy(db: &Connection, id: i64) -> Result<(), Error> {
    let (level, mut number): (i64, String) = db.query_row(
        "SELECT level, number FROM room WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    loop {
        number = crate::annot::next_mark(&number);
        let taken: i64 = db.query_row(
            "SELECT count(*) FROM room WHERE level=?1 AND number=?2 AND id<>?3",
            params![level, number, id],
            |r| r.get(0),
        )?;
        if taken == 0 {
            break;
        }
    }
    db.execute("UPDATE room SET number=?2 WHERE id=?1", params![id, number])?;
    Ok(())
}

impl Model {
    pub fn add_room(&mut self, r: &RoomSpec) -> Result<i64, Error> {
        check(&self.db, r)?;
        self.op("room_add", |db| {
            db.execute(
                "INSERT INTO room(level,name,number,px,py,height) VALUES (?1,?2,?3,?4,?5,?6)",
                params![r.level, r.name, r.number, r.point.0, r.point.1, r.height],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("room", "id", id)))
        })
    }

    /// Several rooms as one model operation: one journal entry, one undo. Empty input: no-op.
    pub fn add_rooms(&mut self, rs: &[RoomSpec]) -> Result<Vec<i64>, Error> {
        if rs.is_empty() {
            return Ok(vec![]);
        }
        for r in rs {
            check(&self.db, r)?;
        }
        self.op("room_add", |db| {
            let mut ids = Vec::with_capacity(rs.len());
            let mut inv = Inv::default();
            for r in rs {
                db.execute(
                    "INSERT INTO room(level,name,number,px,py,height) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![r.level, r.name, r.number, r.point.0, r.point.1, r.height],
                )?;
                let id = db.last_insert_rowid();
                inv = inv.delete("room", "id", id);
                ids.push(id);
            }
            let first = ids[0];
            Ok((ids, first, inv))
        })
    }

    /// Wall groups of the level (two or more connected walls) that bound no region, walls or
    /// room separators included: open loops. Counted for reports, never closed by guess.
    pub fn open_wall_groups(&self, level: i64) -> Result<usize, Error> {
        use ddc_geom::predicates::point_in_ring;
        let elev: f64 = self
            .db
            .query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
                r.get(0)
            })
            .map_err(|_| Error::Invalid(format!("no such level {level}")))?;
        let lw = level_walls(&self.db, level, elev)?;
        let a = arrange_with_lines(&lw.polys, &level_lines(&self.db, level)?, Tol::default());
        let area = |o: &[V2]| -> f64 {
            (0..o.len())
                .map(|i| {
                    let (p, q) = (o[i], o[(i + 1) % o.len()]);
                    p.x * q.y - q.x * p.y
                })
                .sum::<f64>()
                .abs()
        };
        // an island inside a room is a hole of that room, never a separate outline
        let shapes: Vec<&[V2]> = a
            .outlines
            .iter()
            .chain(a.rooms.iter().flat_map(|r| r.holes.iter()))
            .map(Vec::as_slice)
            .collect();
        // each wall belongs to the smallest outline around its footprint (islands inside rooms)
        let mut walls: Vec<std::collections::BTreeSet<i64>> =
            vec![Default::default(); shapes.len()];
        for (wall, _, rings, _, _) in &lw.layers {
            for ring in rings {
                let Some(p) = ddc_geom::rooms::interior_point(&[ring.as_slice()]) else {
                    continue;
                };
                let k = (0..shapes.len())
                    .filter(|&k| point_in_ring(p, shapes[k]) > 0)
                    .min_by(|&i, &j| area(shapes[i]).total_cmp(&area(shapes[j])));
                if let Some(k) = k {
                    walls[k].insert(*wall);
                }
            }
        }
        Ok(shapes
            .iter()
            .copied()
            .zip(&walls)
            .filter(|(o, w)| {
                w.len() >= 2
                    && !a
                        .rooms
                        .iter()
                        .any(|r| r.outer.iter().any(|&v| point_in_ring(v, o) >= 0))
            })
            .count())
    }

    pub fn update_room(&mut self, id: i64, r: &RoomSpec) -> Result<(), Error> {
        check(&self.db, r)?;
        self.op("room_update", |db| {
            let inv = Inv::default().restore(db, "room", "id", id)?;
            let n = db.execute(
                "UPDATE room SET level=?2,name=?3,number=?4,px=?5,py=?6,height=?7 WHERE id=?1",
                params![id, r.level, r.name, r.number, r.point.0, r.point.1, r.height],
            )?;
            if n == 0 {
                return invalid(format!("no such room {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn delete_room(&mut self, id: i64) -> Result<(), Error> {
        self.op("room_delete", |db| {
            let inv = Inv::default().restore(db, "room", "id", id)?;
            let inv = crate::zones::drop_room(db, inv, id)?;
            if db.execute("DELETE FROM room WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such room {id}"));
            }
            Ok(((), id, inv))
        })
    }

    /// Sketched boundary of a room that walls do not close, if it has one.
    pub fn room_outline(&self, id: i64) -> Result<Option<Vec<(f64, f64)>>, Error> {
        let s: Option<String> = self
            .db
            .query_row("SELECT outline FROM room WHERE id=?1", [id], |r| r.get(0))?;
        Ok(s.and_then(|s| serde_json::from_str(&s).ok()))
    }

    /// Moves the room point and, for a sketched room, replaces its boundary: one operation.
    pub fn set_room_shape(
        &mut self,
        id: i64,
        point: (f64, f64),
        outline: Option<&[(f64, f64)]>,
    ) -> Result<(), Error> {
        let finite = |p: &(f64, f64)| p.0.is_finite() && p.1.is_finite();
        if !finite(&point) || outline.is_some_and(|o| o.len() < 3 || !o.iter().all(finite)) {
            return invalid("room shape must be finite, a boundary needs 3 points".into());
        }
        let text = outline.map(|o| serde_json::to_string(o)).transpose()?;
        self.op("room_shape", |db| {
            let inv = Inv::default().restore(db, "room", "id", id)?;
            let n = db.execute(
                "UPDATE room SET px=?2,py=?3,outline=COALESCE(?4,outline) WHERE id=?1",
                params![id, point.0, point.1, text],
            )?;
            if n == 0 {
                return invalid(format!("no such room {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn add_room_sep(&mut self, s: &SepSpec) -> Result<i64, Error> {
        check_sep(&self.db, s)?;
        self.op("room_sep_add", |db| {
            db.execute(
                "INSERT INTO room_sep(level,ax,ay,bx,by) VALUES (?1,?2,?3,?4,?5)",
                params![s.level, s.a.0, s.a.1, s.b.0, s.b.1],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("room_sep", "id", id)))
        })
    }

    pub fn update_room_sep(&mut self, id: i64, s: &SepSpec) -> Result<(), Error> {
        check_sep(&self.db, s)?;
        self.op("room_sep_update", |db| {
            let inv = Inv::default().restore(db, "room_sep", "id", id)?;
            let n = db.execute(
                "UPDATE room_sep SET level=?2,ax=?3,ay=?4,bx=?5,by=?6 WHERE id=?1",
                params![id, s.level, s.a.0, s.a.1, s.b.0, s.b.1],
            )?;
            if n == 0 {
                return invalid(format!("no such room separator {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn delete_room_sep(&mut self, id: i64) -> Result<(), Error> {
        self.op("room_sep_delete", |db| {
            let inv = Inv::default().restore(db, "room_sep", "id", id)?;
            if db.execute("DELETE FROM room_sep WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such room separator {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn room_sep(&self, id: i64) -> Result<SepSpec, Error> {
        self.db
            .query_row(
                "SELECT level,ax,ay,bx,by FROM room_sep WHERE id=?1",
                [id],
                |r| {
                    Ok(SepSpec {
                        level: r.get(0)?,
                        a: (r.get(1)?, r.get(2)?),
                        b: (r.get(3)?, r.get(4)?),
                    })
                },
            )
            .map_err(|_| Error::Invalid(format!("no such room separator {id}")))
    }

    /// Level separators in ascending id order.
    pub fn room_seps(&self, level: i64) -> Result<Vec<(i64, SepSpec)>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id,ax,ay,bx,by FROM room_sep WHERE level=?1 ORDER BY id")?;
        let rows = st.query_map([level], |r| {
            Ok((
                r.get(0)?,
                SepSpec {
                    level,
                    a: (r.get(1)?, r.get(2)?),
                    b: (r.get(3)?, r.get(4)?),
                },
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// What bounds a room: the walls and separators whose faces carry the outline edges
    /// (ascending: walls first, then separators). Empty if not enclosed.
    pub fn room_boundaries(&self, id: i64) -> Result<Vec<Boundary>, Error> {
        let Some(q) = self.room_quantities(id)? else {
            return Ok(vec![]);
        };
        let level: i64 = self
            .db
            .query_row("SELECT level FROM room WHERE id=?1", [id], |r| r.get(0))
            .map_err(|_| Error::Invalid(format!("no such room {id}")))?;
        // (id, a, b, lower and upper faces along the normal to the left of the axis)
        let mut walls: Vec<(i64, V2, V2, f64, f64)> = vec![];
        {
            let mut st = self.db.prepare(
                "SELECT w.id,w.ax,w.ay,w.bx,w.by,w.offset0,                  (SELECT COALESCE(SUM(thickness),0) FROM wall_type_layer l WHERE l.wtype=w.wtype)                  FROM wall w WHERE w.level=?1 ORDER BY w.id",
            )?;
            let rows = st.query_map([level], |r| {
                let total: f64 = r.get(6)?;
                let off0: Option<f64> = r.get(5)?;
                let lo = off0.unwrap_or(-total / 2.0);
                Ok((
                    r.get(0)?,
                    v2(r.get(1)?, r.get(2)?),
                    v2(r.get(3)?, r.get(4)?),
                    lo,
                    lo + total,
                ))
            })?;
            for w in rows {
                walls.push(w?);
            }
        }
        let seps = self.room_seps(level)?;
        const EPS: f64 = 1e-6;
        let mut out: Vec<Boundary> = vec![];
        for ring in &q.rings {
            for i in 0..ring.len() {
                let (p, e) = (ring[i], ring[(i + 1) % ring.len()]);
                let (p, e) = (v2(p.0, p.1), v2(e.0, e.1));
                let d = e - p;
                let dl = d.len();
                if dl <= 0.0 {
                    continue;
                }
                // Overlap of the edge with the object's span along the edge axis (not just the midpoint):
                // collinear walls merge into one long outline edge.
                let ed = v2(d.x / dl, d.y / dl);
                let span = |a: V2, b: V2| -> (f64, f64) {
                    let (s0, s1) = ((a - p).dot(ed), (b - p).dot(ed));
                    (s0.min(s1).max(0.0), s0.max(s1).min(dl))
                };
                let mut wall_iv: Vec<(f64, f64)> = vec![];
                for &(wid, a, b, lo, hi) in &walls {
                    let u = b - a;
                    let len = u.len();
                    if len <= 0.0 || (d.cross(u) / (dl * len)).abs() > 1e-9 {
                        continue;
                    }
                    let u = v2(u.x / len, u.y / len);
                    let n = v2(-u.y, u.x);
                    let t = (p - a).dot(n);
                    if (t - lo).abs() <= EPS || (t - hi).abs() <= EPS {
                        let (s0, s1) = span(a, b);
                        if s1 - s0 > EPS {
                            out.push(Boundary::Wall(wid));
                            wall_iv.push((s0, s1));
                        }
                    }
                }
                wall_iv.sort_by(|x, y| x.0.total_cmp(&y.0));
                for (sid, sp) in &seps {
                    let (a, b) = (v2(sp.a.0, sp.a.1), v2(sp.b.0, sp.b.1));
                    let u = b - a;
                    let len = u.len();
                    if len <= 0.0 || (d.cross(u) / (dl * len)).abs() > 1e-9 {
                        continue;
                    }
                    let u = v2(u.x / len, u.y / len);
                    if (p - a).dot(v2(-u.y, u.x)).abs() > EPS {
                        continue;
                    }
                    let (s0, s1) = span(a, b);
                    // The part of a separator not covered by a wall (a wall takes priority).
                    let mut free = 0.0;
                    let mut cur = s0;
                    for &(w0, w1) in &wall_iv {
                        if w1 <= cur {
                            continue;
                        }
                        if w0 >= s1 {
                            break;
                        }
                        free += (w0.min(s1) - cur).max(0.0);
                        cur = cur.max(w1);
                    }
                    free += (s1 - cur).max(0.0);
                    if free > EPS {
                        out.push(Boundary::Sep(*sid));
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Room quantities; `None` if the room is not enclosed by walls.
    pub fn room_quantities(&self, id: i64) -> Result<Option<RoomQty>, Error> {
        let mut st = self
            .db
            .prepare("SELECT area, perimeter, volume, rings FROM room_qty WHERE room=?1")?;
        let mut rows = st.query([id])?;
        let Some(r) = rows.next()? else {
            return Ok(None);
        };
        let rings: String = r.get(3)?;
        Ok(Some(RoomQty {
            area: r.get(0)?,
            perimeter: r.get(1)?,
            volume: r.get(2)?,
            rings: serde_json::from_str(&rings)?,
        }))
    }

    /// Room diagnostics: (id, text). Empty: every room is enclosed and alone in its region.
    /// A second room is told by the region its point lies in, not by its outline text.
    pub fn room_issues(&self) -> Result<Vec<(i64, String)>, Error> {
        let mut out = vec![];
        let mut st = self.db.prepare(
            "SELECT r.id, r.px, r.py, q.rings, r.level FROM room r LEFT JOIN room_qty q ON q.room=r.id \
             ORDER BY r.id",
        )?;
        let rows: Vec<(i64, f64, f64, Option<String>, i64)> = st
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<_, _>>()?;
        let mut regions: std::collections::HashMap<i64, Vec<Region>> = Default::default();
        let mut first: std::collections::HashMap<(i64, usize), i64> = Default::default();
        let mut first_rings: std::collections::HashMap<(i64, String), i64> = Default::default();
        let mut listed = std::collections::HashSet::new();
        let dup = |id: i64, k: i64| {
            let t = format!("room {id}: same region as room {k}; area counted in room {k}");
            (id, t)
        };
        for (id, x, y, rings, level) in &rows {
            let Some(g) = rings else {
                out.push((
                    *id,
                    format!("room {id} at ({x}, {y}): not enclosed by walls"),
                ));
                continue;
            };
            if !regions.contains_key(level) {
                regions.insert(*level, arrangement(&self.db, *level)?.rooms);
            }
            let held = match regions[level].iter().position(|r| r.contains(v2(*x, *y))) {
                Some(i) => *first.entry((*level, i)).or_insert(*id),
                None => *first_rings.entry((*level, g.clone())).or_insert(*id),
            };
            if held != *id {
                out.push(dup(*id, held));
                listed.insert(*id);
            }
        }
        // whatever the regenerate zeroed is listed too, so the two never disagree
        for (d, k) in self.room_duplicates()? {
            if listed.insert(d) {
                out.push(dup(d, k));
            }
        }
        out.sort_by_key(|e| e.0);
        Ok(out)
    }

    /// Rooms whose region another room already holds: (room, the lowest room id there, which
    /// keeps the quantities). Such a room has its outline but zero area, perimeter and volume.
    pub fn room_duplicates(&self) -> Result<Vec<(i64, i64)>, Error> {
        let mut st = self.db.prepare(
            "SELECT q.room, (SELECT MIN(k.room) FROM room_qty k JOIN room kr ON kr.id=k.room \
               WHERE kr.level=r.level AND k.rings=q.rings AND k.room<q.room) \
             FROM room_qty q JOIN room r ON r.id=q.room \
             WHERE q.area=0 AND q.perimeter=0 ORDER BY q.room",
        )?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
        })?;
        let mut out = vec![];
        for r in rows {
            if let (d, Some(k)) = r? {
                out.push((d, k));
            }
        }
        Ok(out)
    }

    /// Enclosed areas of a level (by walls), independent of placed rooms.
    pub fn room_regions(&self, level: i64) -> Result<Vec<Region>, Error> {
        Ok(arrangement(&self.db, level)?.rooms)
    }

    /// Enclosed wall outlines of a level, ignoring room separators.
    pub fn wall_regions(&self, level: i64) -> Result<Vec<Region>, Error> {
        Ok(wall_arrangement(&self.db, level)?.rooms)
    }

    /// A slab for each enclosed room of a level (outline = inner wall faces).
    /// One operation; an error if there are no rooms.
    pub fn slabs_from_rooms(
        &mut self,
        level: i64,
        slab_type: i64,
        offset: f64,
    ) -> Result<Vec<i64>, Error> {
        let ids: Vec<i64> = {
            let mut st = self.db.prepare(
                "SELECT q.room FROM room_qty q JOIN room r ON r.id=q.room \
                 WHERE r.level=?1 ORDER BY q.room",
            )?;
            let rows = st.query_map([level], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut specs = vec![];
        let mut seen: Vec<Vec<Vec<(f64, f64)>>> = vec![];
        for id in ids {
            let q = self.room_quantities(id)?;
            // One slab per region: the lowest room id of a shared region wins.
            if let Some(q) = &q {
                if seen.contains(&q.rings) {
                    continue;
                }
                seen.push(q.rings.clone());
            }
            let Some((outer, holes)) = q.as_ref().and_then(|q| q.rings.split_first()) else {
                return invalid(format!("room {id}: no boundary"));
            };
            specs.push(SlabSpec {
                level,
                slab_type,
                offset,
                outer: outer.clone(),
                holes: holes.to_vec(),
            });
        }
        if specs.is_empty() {
            return invalid(format!("level {level}: no enclosed rooms"));
        }
        self.add_slabs(&specs)
    }

    /// A slab on the outer outline of each wall group of a level (outer faces).
    pub fn slabs_from_outline(
        &mut self,
        level: i64,
        slab_type: i64,
        offset: f64,
    ) -> Result<Vec<i64>, Error> {
        let a = arrangement(&self.db, level)?;
        let specs: Vec<SlabSpec> = a
            .outlines
            .iter()
            .map(|o| to_spec(level, slab_type, offset, o, &[]))
            .collect();
        if specs.is_empty() {
            return invalid(format!("level {level}: no walls"));
        }
        for s in &specs {
            profile(&s.outer, &s.holes)?;
        }
        self.add_slabs(&specs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, OpeningSpec, Swing, WallSpec};

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    /// Walls along the axis polygon `p` (+ extra segments), thickness `th`, height 3.
    fn plan(
        m: &mut Model,
        p: &[(f64, f64)],
        extra: &[((f64, f64), (f64, f64))],
        th: f64,
    ) -> (i64, Vec<i64>) {
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: th,
                }],
            )
            .unwrap();
        let mut axes: Vec<_> = (0..p.len()).map(|i| (p[i], p[(i + 1) % p.len()])).collect();
        axes.extend_from_slice(extra);
        let specs: Vec<WallSpec> = axes
            .iter()
            .map(|&(a, b)| WallSpec {
                a,
                b,
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .collect();
        let ids = m.add_walls(&specs).unwrap();
        (lv, ids)
    }

    fn room(m: &mut Model, lv: i64, p: (f64, f64)) -> i64 {
        m.add_room(&RoomSpec {
            level: lv,
            name: "Room".into(),
            number: "1".into(),
            point: p,
            height: 3.0,
        })
        .unwrap()
    }

    const R64: [(f64, f64); 4] = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];

    /// The same walls on the second level (the wall type is already created by `plan`).
    fn second_level(m: &mut Model) -> i64 {
        let l2 = m.add_level("L2", 3.0).unwrap();
        let wt: i64 =
            m.db.query_row("SELECT id FROM wall_type", [], |r| r.get(0))
                .unwrap();
        let specs: Vec<WallSpec> = (0..4)
            .map(|i| WallSpec {
                a: R64[i],
                b: R64[(i + 1) % 4],
                level: l2,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .collect();
        m.add_walls(&specs).unwrap();
        l2
    }

    #[test]
    fn same_layout_on_two_levels_is_not_duplicate_region() {
        let mut m = Model::open(":memory:").unwrap();
        let (l1, _) = plan(&mut m, &R64, &[], 0.3);
        let l2 = second_level(&mut m);
        room(&mut m, l1, (3., 2.));
        room(&mut m, l2, (3., 2.));
        assert_eq!(m.room_issues().unwrap(), vec![]);
        room(&mut m, l2, (2., 2.));
        let v = m.room_issues().unwrap();
        assert_eq!(v.len(), 1, "{v:?}");
    }

    #[test]
    fn non_finite_numbers_are_rejected() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, _) = plan(&mut m, &R64, &[], 0.3);
        let spec = |h: f64| RoomSpec {
            level: lv,
            name: "r".into(),
            number: "1".into(),
            height: h,
            point: (3., 2.),
        };
        assert!(m.add_room(&spec(f64::INFINITY)).is_err());
        assert!(m.add_room(&spec(f64::NAN)).is_err());
        assert!(m.add_level("x", f64::INFINITY).is_err());
        let layer = |t: f64| {
            [LayerSpec {
                material: "c".into(),
                thickness: t,
            }]
        };
        assert!(m.add_slab_type("s", &layer(f64::INFINITY)).is_err());
        assert!(m.add_wall_type("w", &layer(f64::INFINITY)).is_err());
    }

    #[test]
    fn slabs_from_rooms_one_slab_per_region() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, _) = plan(&mut m, &R64, &[], 0.3);
        room(&mut m, lv, (3., 2.));
        room(&mut m, lv, (2., 2.));
        let st = m
            .add_slab_type(
                "f",
                &[LayerSpec {
                    material: "c".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let ids = m.slabs_from_rooms(lv, st, 0.0).unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn room_6x4_walls_0_3() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, _) = plan(&mut m, &R64, &[], 0.3);
        let r = room(&mut m, lv, (3., 2.));
        let q = m.room_quantities(r).unwrap().unwrap();
        near(q.area, 21.09);
        near(q.perimeter, 18.8);
        near(q.volume, 63.27);
        assert_eq!(q.rings.len(), 1);
        assert_eq!(q.rings[0].len(), 4);
        assert!(m.room_issues().unwrap().is_empty());
        m.assert_integrity();
    }

    #[test]
    fn l_plan_internal_wall_two_rooms() {
        let mut m = Model::open(":memory:").unwrap();
        let p = [(0., 0.), (8., 0.), (8., 4.), (4., 4.), (4., 8.), (0., 8.)];
        let (lv, _) = plan(&mut m, &p, &[((4., 0.), (4., 4.))], 0.2);
        assert_eq!(m.room_regions(lv).unwrap().len(), 2);
        let left = room(&mut m, lv, (1., 6.));
        let right = room(&mut m, lv, (6., 2.));
        let (ql, qr) = (
            m.room_quantities(left).unwrap().unwrap(),
            m.room_quantities(right).unwrap().unwrap(),
        );
        near(ql.area, 29.64);
        near(ql.perimeter, 23.2);
        near(qr.area, 14.44);
        near(qr.perimeter, 15.2);
    }

    #[test]
    fn gap_means_no_room_with_diagnostic_and_wall_edit_recomputes() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, ids) = plan(&mut m, &R64, &[], 0.3);
        let r = room(&mut m, lv, (3., 2.));
        m.set_wall_axis(ids[0], (0., 0.), (5.5, 0.)).unwrap();
        assert_eq!(m.room_quantities(r).unwrap(), None);
        let iss = m.room_issues().unwrap();
        assert_eq!(iss.len(), 1);
        assert_eq!(iss[0].0, r);
        assert!(iss[0].1.contains("not enclosed"), "{iss:?}");
        m.undo().unwrap();
        near(m.room_quantities(r).unwrap().unwrap().area, 21.09);
        // Deleting a wall also opens the room; undo restores it.
        m.delete_wall(ids[2]).unwrap();
        assert_eq!(m.room_quantities(r).unwrap(), None);
        m.undo().unwrap();
        assert!(m.room_quantities(r).unwrap().is_some());
        m.assert_integrity();
    }

    #[test]
    fn duplicate_room_and_level_reference() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, _) = plan(&mut m, &R64, &[], 0.3);
        let a = room(&mut m, lv, (3., 2.));
        let b = room(&mut m, lv, (1., 1.));
        let iss = m.room_issues().unwrap();
        assert_eq!(
            iss,
            vec![(b, format!("room {b}: same region as room {a}; area counted in room {a}"))]
        );
        assert!(m
            .add_room(&RoomSpec {
                level: lv,
                name: "x".into(),
                number: "2".into(),
                point: (1., 1.),
                height: 0.0
            })
            .is_err());
        m.delete_room(b).unwrap();
        assert!(m.room_issues().unwrap().is_empty());
        let e = m.delete_level(lv).unwrap_err().to_string();
        assert!(e.contains(&format!("room {a}")), "{e}");
    }

    fn level_room_area(m: &Model, lv: i64) -> (f64, f64) {
        let t = m.level_totals().unwrap();
        let t = t.iter().find(|t| t.level == lv).unwrap();
        (t.totals.room_area, t.totals.room_volume)
    }

    #[test]
    fn two_rooms_one_region_count_area_once() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, _) = plan(&mut m, &R64, &[], 0.3);
        let a = room(&mut m, lv, (3., 2.));
        let b = room(&mut m, lv, (1., 1.));
        let qa = m.room_quantities(a).unwrap().unwrap();
        near(qa.area, 21.09);
        near(qa.volume, 21.09 * 3.0);
        let qb = m.room_quantities(b).unwrap().unwrap();
        assert_eq!((qb.area, qb.perimeter, qb.volume), (0.0, 0.0, 0.0));
        // the outline stays, so the room still draws and tags
        assert_eq!(qb.rings, qa.rings);
        let (area, vol) = level_room_area(&m, lv);
        near(area, 21.09);
        near(vol, 21.09 * 3.0);
        assert_eq!(m.room_duplicates().unwrap(), vec![(b, a)]);
        m.assert_integrity();
    }

    #[test]
    fn duplicate_room_listed_in_room_issues() {
        let mut m = Model::open(":memory:").unwrap();
        // two regions, split by a wall at x=3
        let (lv, _) = plan(&mut m, &R64, &[((3., 0.), (3., 4.))], 0.2);
        let a = room(&mut m, lv, (1., 2.));
        let c = room(&mut m, lv, (4.5, 2.));
        assert!(m.room_issues().unwrap().is_empty());
        let b = room(&mut m, lv, (2., 1.));
        let d = room(&mut m, lv, (5., 1.));
        let iss = m.room_issues().unwrap();
        let ids: Vec<i64> = iss.iter().map(|i| i.0).collect();
        assert_eq!(ids, vec![b, d], "{iss:?}");
        assert!(iss[0].1.contains(&format!("same region as room {a}")), "{iss:?}");
        assert!(iss[1].1.contains(&format!("same region as room {c}")), "{iss:?}");
        let both = m.room_quantities(a).unwrap().unwrap().area
            + m.room_quantities(c).unwrap().unwrap().area;
        near(level_room_area(&m, lv).0, both);
    }

    #[test]
    fn delete_first_room_moves_quantities_to_the_other_and_undo_restores() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, _) = plan(&mut m, &R64, &[], 0.3);
        let a = room(&mut m, lv, (3., 2.));
        let b = room(&mut m, lv, (1., 1.));
        m.delete_room(a).unwrap();
        near(m.room_quantities(b).unwrap().unwrap().area, 21.09);
        near(level_room_area(&m, lv).0, 21.09);
        assert!(m.room_duplicates().unwrap().is_empty());
        assert!(m.room_issues().unwrap().is_empty());
        m.undo().unwrap();
        near(m.room_quantities(a).unwrap().unwrap().area, 21.09);
        assert_eq!(m.room_quantities(b).unwrap().unwrap().area, 0.0);
        near(level_room_area(&m, lv).0, 21.09);
        assert_eq!(m.room_duplicates().unwrap(), vec![(b, a)]);
        m.delete_room(a).unwrap();
        near(m.room_quantities(b).unwrap().unwrap().area, 21.09);
        m.assert_integrity();
    }

    fn door(along: f64) -> OpeningSpec {
        OpeningSpec {
            along,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        }
    }

    #[test]
    fn swing_is_stored_updated_and_undone() {
        let mut m = Model::open(":memory:").unwrap();
        let (_, ids) = plan(&mut m, &R64, &[], 0.3);
        let d = m.add_opening(ids[0], &door(1.0)).unwrap();
        assert_eq!(m.opening_swing(d).unwrap(), Swing::default());
        let sw = Swing {
            side: true,
            hinge: true,
        };
        let e = m.add_opening_with(ids[0], &door(3.0), sw).unwrap();
        assert_eq!(m.opening_swing(e).unwrap(), sw);
        // dimension edit keeps the swing; explicit edit replaces it in one operation
        m.update_opening(e, &door(3.5)).unwrap();
        assert_eq!(m.opening_swing(e).unwrap(), sw);
        m.update_opening_with(
            e,
            &door(3.2),
            Swing {
                side: false,
                hinge: true,
            },
        )
        .unwrap();
        assert!(m.opening_swing(e).unwrap().hinge && !m.opening_swing(e).unwrap().side);
        m.undo().unwrap();
        assert_eq!(m.opening_swing(e).unwrap(), sw);
        m.set_opening_swing(d, sw).unwrap();
        assert_eq!(m.opening_swing(d).unwrap(), sw);
        m.undo().unwrap();
        assert_eq!(m.opening_swing(d).unwrap(), Swing::default());
        // delete + undo restores the stored swing
        m.delete_opening(e).unwrap();
        m.undo().unwrap();
        assert_eq!(m.opening_swing(e).unwrap(), sw);
        assert!(m.opening_swing(999).is_err());
        assert!(m.set_opening_swing(999, sw).is_err());
        m.assert_integrity();
    }

    const R84: [(f64, f64); 4] = [(0., 0.), (8., 0.), (8., 4.), (0., 4.)];

    #[test]
    fn curtain_wall_axis_bounds_rooms() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, _) = plan(&mut m, &R84, &[], 0.2);
        let (l, r) = (room(&mut m, lv, (1., 2.)), room(&mut m, lv, (6., 2.)));
        near(m.room_quantities(l).unwrap().unwrap().area, 29.64);
        let cw = m
            .add_curtain_wall(&crate::CurtainSpec {
                level: lv,
                name: "CW".into(),
                a: (4., 0.),
                b: (4., 4.),
                bulge: 0.0,
                base_offset: 0.0,
                height: 3.0,
                u: crate::GridLines::Spacing(1.0),
                v: crate::GridLines::Spacing(1.0),
                interior: crate::MullionSection::None,
                border: crate::MullionSection::None,
                lines: Default::default(),
                top: vec![],
            })
            .unwrap();
        near(m.room_quantities(l).unwrap().unwrap().area, 14.82);
        near(m.room_quantities(r).unwrap().unwrap().area, 14.82);
        m.delete_curtain_wall(cw).unwrap();
        near(m.room_quantities(l).unwrap().unwrap().area, 29.64);
        m.undo().unwrap();
        near(m.room_quantities(r).unwrap().unwrap().area, 14.82);
    }

    #[test]
    fn room_sep_splits_region_undo_and_boundaries() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, ids) = plan(&mut m, &R84, &[], 0.2);
        let (l, r) = (room(&mut m, lv, (1., 2.)), room(&mut m, lv, (6., 2.)));
        // one region: both rooms share it
        near(m.room_quantities(l).unwrap().unwrap().area, 29.64);
        assert_eq!(m.room_issues().unwrap().len(), 1);
        // dangling segment changes nothing
        let spur = SepSpec {
            level: lv,
            a: (4., 0.),
            b: (4., 1.5),
        };
        let sp = m.add_room_sep(&spur).unwrap();
        near(m.room_quantities(l).unwrap().unwrap().area, 29.64);
        m.delete_room_sep(sp).unwrap();
        let s = m
            .add_room_sep(&SepSpec {
                level: lv,
                a: (4., 0.),
                b: (4., 4.),
            })
            .unwrap();
        let (ql, qr) = (
            m.room_quantities(l).unwrap().unwrap(),
            m.room_quantities(r).unwrap().unwrap(),
        );
        near(ql.area, 14.82);
        near(qr.area, 14.82);
        near(ql.perimeter, 15.4);
        assert!(m.room_issues().unwrap().is_empty());
        let w = |k: usize| Boundary::Wall(ids[k]);
        assert_eq!(
            m.room_boundaries(l).unwrap(),
            vec![w(0), w(2), w(3), Boundary::Sep(s)]
        );
        assert_eq!(
            m.room_boundaries(r).unwrap(),
            vec![w(0), w(1), w(2), Boundary::Sep(s)]
        );
        assert_eq!(m.room_seps(lv).unwrap().len(), 1);
        m.update_room_sep(
            s,
            &SepSpec {
                level: lv,
                a: (2., 0.),
                b: (2., 4.),
            },
        )
        .unwrap();
        near(m.room_quantities(l).unwrap().unwrap().area, 1.9 * 3.8);
        m.undo().unwrap();
        near(m.room_quantities(l).unwrap().unwrap().area, 14.82);
        m.undo().unwrap();
        near(m.room_quantities(l).unwrap().unwrap().area, 29.64);
        assert!(m.room_seps(lv).unwrap().is_empty());
        m.add_room_sep(&SepSpec {
            level: lv,
            a: (4., 0.),
            b: (4., 4.),
        })
        .unwrap();
        near(m.room_quantities(r).unwrap().unwrap().area, 14.82);
        // bad input
        for bad in [
            SepSpec {
                level: lv,
                a: (1., 1.),
                b: (1., 1.),
            },
            SepSpec {
                level: 99,
                a: (0., 0.),
                b: (1., 0.),
            },
            SepSpec {
                level: lv,
                a: (f64::NAN, 0.),
                b: (1., 0.),
            },
        ] {
            assert!(m.add_room_sep(&bad).is_err());
        }
        // a level with separators cannot be deleted
        let e = m.delete_level(lv).unwrap_err().to_string();
        assert!(e.contains("room_sep"), "{e}");
        m.assert_integrity();
    }

    #[test]
    fn room_boundaries_of_closed_room_and_open_room() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, ids) = plan(&mut m, &R64, &[], 0.3);
        let a = room(&mut m, lv, (3., 2.));
        assert_eq!(
            m.room_boundaries(a).unwrap(),
            ids.iter().map(|&i| Boundary::Wall(i)).collect::<Vec<_>>()
        );
        let o = room(&mut m, lv, (10., 10.));
        assert!(m.room_boundaries(o).unwrap().is_empty());
        assert!(m.room_boundaries(999).unwrap().is_empty());
    }

    #[test]
    fn split_collinear_walls_all_pieces_are_boundaries() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, ids) = plan(&mut m, &R84, &[], 0.2);
        let a = room(&mut m, lv, (4., 2.));
        let p1 = m.split_wall(ids[0], 0.25).unwrap();
        let b = m.room_boundaries(a).unwrap();
        assert!(b.contains(&Boundary::Wall(ids[0])) && b.contains(&Boundary::Wall(p1)));
        assert_eq!(b.len(), ids.len() + 1);
        let p2 = m.split_wall(p1, 3.0).unwrap();
        let b = m.room_boundaries(a).unwrap();
        for w in [ids[0], p1, p2] {
            assert!(b.contains(&Boundary::Wall(w)), "{w}");
        }
        assert_eq!(b.len(), ids.len() + 2);
    }

    #[test]
    fn separator_on_wall_face_yields_wall_only() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, ids) = plan(&mut m, &R84, &[], 0.2);
        let a = room(&mut m, lv, (4., 2.));
        let sep = m
            .add_room_sep(&SepSpec {
                level: lv,
                a: (0., 0.1),
                b: (8., 0.1),
            })
            .unwrap();
        let b = m.room_boundaries(a).unwrap();
        assert!(!b.contains(&Boundary::Sep(sep)), "{b:?}");
        assert!(b.contains(&Boundary::Wall(ids[0])));
    }

    #[test]
    fn collinear_separators_chain_are_all_boundaries() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, _) = plan(&mut m, &R84, &[], 0.2);
        let (l, _r) = (room(&mut m, lv, (1., 2.)), room(&mut m, lv, (6., 2.)));
        let mk = |m: &mut Model, a, b| m.add_room_sep(&SepSpec { level: lv, a, b }).unwrap();
        let s1 = mk(&mut m, (4., 0.1), (4., 1.5));
        let s2 = mk(&mut m, (4., 1.5), (4., 2.5));
        let s3 = mk(&mut m, (4., 2.5), (4., 3.9));
        let b = m.room_boundaries(l).unwrap();
        for s in [s1, s2, s3] {
            assert!(b.contains(&Boundary::Sep(s)), "{s} {b:?}");
        }
    }

    #[test]
    fn auto_slabs_from_rooms_and_outline() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, _) = plan(&mut m, &R64, &[], 0.3);
        let t = m
            .add_slab_type(
                "floor",
                &[LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        assert!(m.slabs_from_rooms(lv, t, 0.0).is_err());
        room(&mut m, lv, (3., 2.));
        let s = m.slabs_from_rooms(lv, t, 0.0).unwrap();
        assert_eq!(s.len(), 1);
        near(m.slab_quantities(s[0]).unwrap()[0].volume, 21.09 * 0.2);
        let o = m.slabs_from_outline(lv, t, 0.0).unwrap();
        assert_eq!(o.len(), 1);
        near(m.slab_quantities(o[0]).unwrap()[0].area, 6.3 * 4.3);
        m.assert_integrity();
        m.undo().unwrap();
        m.undo().unwrap();
        let n: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM slab", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    /// Straight walls on `lv` with the wall type `plan` created.
    fn segs(m: &mut Model, lv: i64, s: &[((f64, f64), (f64, f64))]) {
        let wall_type: i64 =
            m.db.query_row("SELECT id FROM wall_type", [], |r| r.get(0))
                .unwrap();
        let specs: Vec<WallSpec> = s
            .iter()
            .map(|&(a, b)| WallSpec {
                a,
                b,
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type,
                offset0: None,
            })
            .collect();
        m.add_walls(&specs).unwrap();
    }

    #[test]
    fn add_rooms_is_one_undo_step() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, _) = plan(&mut m, &R84, &[((4., 0.), (4., 4.))], 0.2);
        let spec = |n: &str, x: f64| RoomSpec {
            level: lv,
            name: format!("Room {n}"),
            number: n.into(),
            point: (x, 2.),
            height: 3.0,
        };
        let count = |m: &Model| -> i64 {
            m.db.query_row("SELECT count(*) FROM room", [], |r| r.get(0))
                .unwrap()
        };
        assert!(m.add_rooms(&[]).unwrap().is_empty());
        let ids = m.add_rooms(&[spec("1", 2.), spec("2", 6.)]).unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(count(&m), 2);
        assert!(m.room_quantities(ids[1]).unwrap().is_some(), "closed room");
        m.undo().unwrap();
        assert_eq!(count(&m), 0, "one undo removes the whole batch");
        let mut bad = spec("3", 2.);
        bad.height = f64::NAN;
        assert!(m.add_rooms(&[spec("1", 2.), bad]).is_err());
        assert_eq!(count(&m), 0, "a bad spec inserts nothing");
        m.assert_integrity();
    }

    #[test]
    fn open_wall_groups_counts_unclosed_chains_only() {
        let mut m = Model::open(":memory:").unwrap();
        let (lv, _) = plan(&mut m, &R64, &[], 0.2);
        assert_eq!(m.open_wall_groups(lv).unwrap(), 0, "closed box");
        // a lone wall is not a loop
        segs(&mut m, lv, &[((20., 0.), (24., 0.))]);
        assert_eq!(m.open_wall_groups(lv).unwrap(), 0);
        // U shape: three walls, one side open
        segs(
            &mut m,
            lv,
            &[((10., 0.), (14., 0.)), ((14., 0.), (14., 4.)), ((10., 0.), (10., 4.))],
        );
        assert_eq!(m.open_wall_groups(lv).unwrap(), 1);
        // a separator closes it: no longer open
        m.add_room_sep(&SepSpec {
            level: lv,
            a: (10., 4.),
            b: (14., 4.),
        })
        .unwrap();
        assert_eq!(m.open_wall_groups(lv).unwrap(), 0);
        // an open L island inside the closed box is its own open group
        segs(&mut m, lv, &[((1., 1.), (2., 1.)), ((2., 1.), (2., 2.))]);
        assert_eq!(m.open_wall_groups(lv).unwrap(), 1);
        assert!(m.open_wall_groups(999).is_err());
    }
}
