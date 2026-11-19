//! `room_sep`: the fire separation of a tall building. A compartment (a floor) is not bigger
//! than the rule allows, every boundary of the core (floor slabs above the first level, walls on
//! the shaft edges, doors in them) carries a fire rating, and at least `stairs_min` stairs stand
//! in a shaft whose walls are all rated.

use super::case::GeomO;
use super::facts::rows;
use super::oracle::{no_capability, num, result, EvalCx, Oracle, OracleResult};
use ddc_model::Model;

/// Distance from a wall end to a shaft edge for the wall to count as the shaft's wall, m.
const ON_EDGE: f64 = 0.35;

pub struct Wall {
    pub id: i64,
    pub level: i64,
    pub a: [f64; 2],
    pub b: [f64; 2],
    pub rated: bool,
}

pub struct Door {
    pub id: i64,
    pub wall: i64,
    pub rated: bool,
}

pub struct Shaft {
    pub level: i64,
    pub top: i64,
    pub poly: Vec<[f64; 2]>,
}

#[derive(Default)]
pub struct Fire {
    /// (level id, elevation order, plan area of its slabs)
    pub levels: Vec<(i64, usize, f64)>,
    pub slabs: Vec<(i64, bool)>,
    pub walls: Vec<Wall>,
    pub doors: Vec<Door>,
    pub shafts: Vec<Shaft>,
    /// (level, x, y)
    pub stairs: Vec<(i64, f64, f64)>,
}

/// A rating is a value that says something: not empty, not "0" or "none".
pub fn is_rated(v: &str) -> bool {
    !matches!(v.trim().to_lowercase().as_str(), "" | "0" | "none" | "no" | "false" | "-" | "n/a")
}

fn dist_seg(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 < 1e-12 { 0.0 } else { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) };
    (p[0] - (a[0] + t * dx)).hypot(p[1] - (a[1] + t * dy))
}

fn inside(p: [f64; 2], poly: &[[f64; 2]]) -> bool {
    let mut c = false;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
    }
    c
}

impl Shaft {
    fn spans(&self, level: i64, order: &dyn Fn(i64) -> usize) -> bool {
        (order(self.level)..=order(self.top)).contains(&order(level))
    }

    fn edge_wall(&self, w: &Wall, order: &dyn Fn(i64) -> usize) -> bool {
        self.spans(w.level, order)
            && (0..self.poly.len()).any(|i| {
                let (p, q) = (self.poly[i], self.poly[(i + 1) % self.poly.len()]);
                dist_seg(w.a, p, q) <= ON_EDGE && dist_seg(w.b, p, q) <= ON_EDGE
            })
    }
}

/// The verdict and the numbers behind it.
pub fn measure(f: &Fire, area_max: f64, stairs_min: usize) -> (String, bool) {
    let order = |l: i64| f.levels.iter().find(|x| x.0 == l).map_or(0, |x| x.1);
    let first = f.levels.iter().map(|x| x.1).min().unwrap_or(0);
    let worst = f.levels.iter().map(|x| x.2).fold(0.0, f64::max);
    let slabs: Vec<_> = f.slabs.iter().filter(|s| order(s.0) > first).collect();
    let slab_ok = slabs.iter().filter(|s| s.1).count();
    let mut edge_walls = vec![];
    let mut shaft_walls: Vec<Vec<&Wall>> = vec![];
    for s in &f.shafts {
        let ws: Vec<&Wall> = f.walls.iter().filter(|w| s.edge_wall(w, &order)).collect();
        edge_walls.extend(ws.iter().copied());
        shaft_walls.push(ws);
    }
    edge_walls.sort_by_key(|w| w.id);
    edge_walls.dedup_by_key(|w| w.id);
    let walls_ok = edge_walls.iter().filter(|w| w.rated).count();
    let doors: Vec<_> = f.doors.iter().filter(|d| edge_walls.iter().any(|w| w.id == d.wall)).collect();
    let doors_ok = doors.iter().filter(|d| d.rated).count();
    let protected = f
        .shafts
        .iter()
        .zip(&shaft_walls)
        .filter(|(s, ws)| {
            !ws.is_empty()
                && ws.iter().all(|w| w.rated)
                && f.stairs.iter().any(|(l, x, y)| s.spans(*l, &order) && inside([*x, *y], &s.poly))
        })
        .count();
    let mut notes = vec![];
    if let Some(w) = edge_walls.iter().find(|w| !w.rated) {
        notes.push(format!("wall {} on a shaft edge has no rating", w.id));
    }
    if let Some(d) = doors.iter().find(|d| !d.rated) {
        notes.push(format!("door {} in a core wall has no rating", d.id));
    }
    let shown = format!(
        "largest compartment {worst:.0} m2, slabs rated {slab_ok}/{}, core walls rated {walls_ok}/{}, core doors rated {doors_ok}/{}, protected stairs {protected}{}",
        slabs.len(),
        edge_walls.len(),
        doors.len(),
        if notes.is_empty() { String::new() } else { format!("; {}", notes.join("; ")) }
    );
    let ok = worst <= area_max
        && !f.levels.is_empty()
        && slab_ok == slabs.len()
        && !edge_walls.is_empty()
        && walls_ok == edge_walls.len()
        && doors_ok == doors.len()
        && protected >= stairs_min;
    (shown, ok)
}

fn text_of(v: rusqlite::types::Value) -> String {
    use rusqlite::types::Value as V;
    match v {
        V::Text(s) => s,
        V::Integer(i) => i.to_string(),
        V::Real(r) => r.to_string(),
        _ => String::new(),
    }
}

pub fn read(m: &Model) -> Result<Fire, String> {
    let db = m.sql();
    let rated = |kind: &str, key: &str| -> Result<std::collections::HashSet<i64>, String> {
        let v = rows(db, &format!("SELECT ref, value FROM prop WHERE kind = '{kind}' AND key = '{key}'"), |r| {
            Ok((r.get::<_, i64>(0)?, text_of(r.get::<_, rusqlite::types::Value>(1)?)))
        })?;
        Ok(v.into_iter().filter(|(_, t)| is_rated(t)).map(|(i, _)| i).collect())
    };
    let mut f = Fire::default();
    let totals = m.level_totals().map_err(|e| format!("takeoff: {e:?}"))?;
    for (i, l) in totals.iter().enumerate() {
        f.levels.push((l.level, i, l.totals.slab_area));
    }
    let slab_rated = rated("slab", "Pset_SlabCommon.FireRating")?;
    f.slabs = rows(db, "SELECT id, level FROM slab", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
        .into_iter()
        .map(|(id, level)| (level, slab_rated.contains(&id)))
        .collect();
    let wall_rated = rated("wall", "Pset_WallCommon.FireRating")?;
    f.walls = rows(db, "SELECT id, level, ax, ay, bx, by FROM wall", |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, [r.get::<_, f64>(2)?, r.get::<_, f64>(3)?], [r.get::<_, f64>(4)?, r.get::<_, f64>(5)?]))
    })?
    .into_iter()
    .map(|(id, level, a, b)| Wall { id, level, a, b, rated: wall_rated.contains(&id) })
    .collect();
    let door_rated = rated("opening", "Pset_DoorCommon.FireRating")?;
    f.doors = rows(db, "SELECT id, wall FROM opening WHERE kind LIKE '%door%'", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
        .into_iter()
        .map(|(id, wall)| Door { id, wall, rated: door_rated.contains(&id) })
        .collect();
    let sh = rows(db, "SELECT id, level, top_level FROM shaft", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))?;
    for (id, level, top) in sh {
        let poly = rows(db, &format!("SELECT x, y FROM shaft_vertex WHERE shaft = {id} ORDER BY idx"), |r| Ok([r.get::<_, f64>(0)?, r.get::<_, f64>(1)?]))?;
        f.shafts.push(Shaft { level, top, poly });
    }
    f.stairs = rows(db, "SELECT level, x, y FROM stair", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?)))?;
    Ok(f)
}

pub fn build(g: &GeomO) -> Option<Box<dyn Oracle>> {
    (g.kind == "room_sep").then(|| Box::new(RoomSep(g.clone())) as Box<dyn Oracle>)
}

struct RoomSep(GeomO);

impl Oracle for RoomSep {
    fn name(&self) -> String {
        "geom room_sep".to_string()
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let (area, stairs) = (num(&self.0.p, "area_max").unwrap_or(2500.0), num(&self.0.p, "stairs_min").unwrap_or(2.0));
        let exp = format!("compartment <= {area} m2, every core boundary rated, >= {stairs} protected stairs");
        match read(&cx.ed.model) {
            Ok(f) => {
                let (shown, ok) = measure(&f, area, stairs as usize);
                result(&self.name(), exp, shown, ok)
            }
            Err(e) => no_capability(&self.name(), exp, &e),
        }
    }
}
