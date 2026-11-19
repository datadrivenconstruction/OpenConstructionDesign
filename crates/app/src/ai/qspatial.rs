//! `query_spatial`: read-only spatial questions over element bounds of the scratch copy.
//! Lengths are metres. Hits are ordered by distance, then id, then category, and cut to
//! `limit`; the reply says how many more exist, so a model never pays for the whole model.

use super::fmt::{self, Reply};
use super::rows;
use crate::editor::Editor;
use ddc_ai::registry::{CmdError, ErrorCode};
use ddc_geom::predicates::point_in_ring;
use ddc_geom::V2;
use serde_json::{json, Value as J};

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;
const DEFAULT_RAY_MAX: f64 = 100.0;
const MODES: [&str; 5] = ["near", "inside", "intersects", "on_level", "ray"];

/// One element with its axis-aligned bounds in metres.
#[derive(Clone, Debug, PartialEq)]
pub struct El {
    pub cat: &'static str,
    pub id: i64,
    pub level: i64,
    pub lo: [f64; 3],
    pub hi: [f64; 3],
}

/// A room: outer ring first, then holes; metres.
#[derive(Clone, Debug, PartialEq)]
pub struct Room {
    pub id: i64,
    pub level: i64,
    pub rings: Vec<Vec<(f64, f64)>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Space {
    pub els: Vec<El>,
    pub rooms: Vec<Room>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub cat: &'static str,
    pub id: i64,
    /// Metres: gap for `near`, distance along the ray for `ray`, zero otherwise.
    pub d: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub mode: &'static str,
    pub total: usize,
    pub hits: Vec<Hit>,
}

/// Bounds of the elements that have a body, and the rooms with a boundary.
pub fn space_of(ed: &Editor) -> Result<Space, String> {
    let all = rows::rows(ed);
    let mut sp = Space::default();
    let mut keys = vec![];
    let mut meta = vec![];
    for r in all.iter() {
        if r.cat == "room" {
            if let Some(q) = ed.model.room_quantities(r.id()).map_err(|e| e.to_string())? {
                sp.rooms.push(Room {
                    id: r.id(),
                    level: r.level,
                    rings: q.rings,
                });
            }
            continue;
        }
        let kind = match r.cat {
            "wall" => "wall",
            "floor" => "slab",
            "column" => "column",
            "beam" => "beam",
            "mep" => "duct",
            _ => continue,
        };
        keys.push((kind.to_string(), r.id()));
        meta.push((r.cat, r.id(), r.level));
    }
    let bounds = ed
        .model
        .clash_element_bounds(&keys)
        .map_err(|e| e.to_string())?;
    for ((cat, id, level), b) in meta.into_iter().zip(bounds) {
        if let Some(b) = b {
            sp.els.push(El {
                cat,
                id,
                level,
                lo: [b.min.x, b.min.y, b.min.z],
                hi: [b.max.x, b.max.y, b.max.z],
            });
        }
    }
    Ok(sp)
}

pub(crate) fn parse_ref(s: &str) -> Result<(&str, i64), String> {
    let bad = || format!("bad reference: {}", fmt::text(s));
    let (cat, id) = s.split_once(':').ok_or_else(bad)?;
    Ok((cat, id.parse::<i64>().map_err(|_| bad())?))
}

fn vec3(v: &J, what: &str) -> Result<[f64; 3], String> {
    let bad = || format!("{what} must be [x, y, z] in metres");
    let a = v.as_array().filter(|a| a.len() == 3).ok_or_else(bad)?;
    let mut out = [0.0; 3];
    for (o, x) in out.iter_mut().zip(a) {
        *o = x.as_f64().filter(|x| x.is_finite()).ok_or_else(bad)?;
    }
    Ok(out)
}

fn box_of(v: &J, what: &str) -> Result<([f64; 3], [f64; 3]), String> {
    let a = vec3(&v["min"], &format!("{what}.min"))?;
    let b = vec3(&v["max"], &format!("{what}.max"))?;
    let lo = [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])];
    let hi = [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])];
    Ok((lo, hi))
}

/// Gap between two boxes; zero when they touch or overlap.
fn gap(alo: [f64; 3], ahi: [f64; 3], blo: [f64; 3], bhi: [f64; 3]) -> f64 {
    let mut s = 0.0;
    for i in 0..3 {
        let g = (blo[i] - ahi[i]).max(alo[i] - bhi[i]).max(0.0);
        s += g * g;
    }
    f64::sqrt(s)
}

fn ray_t(o: [f64; 3], d: [f64; 3], lo: [f64; 3], hi: [f64; 3], tmax: f64) -> Option<f64> {
    let (mut t0, mut t1) = (0.0f64, tmax);
    for i in 0..3 {
        if d[i].abs() < 1e-12 {
            if o[i] < lo[i] || o[i] > hi[i] {
                return None;
            }
        } else {
            let (a, b) = ((lo[i] - o[i]) / d[i], (hi[i] - o[i]) / d[i]);
            t0 = t0.max(a.min(b));
            t1 = t1.min(a.max(b));
            if t0 > t1 {
                return None;
            }
        }
    }
    Some(t0)
}

pub(crate) fn inside_ring(rings: &[Vec<(f64, f64)>], x: f64, y: f64) -> bool {
    let ring = |r: &Vec<(f64, f64)>| -> Vec<V2> { r.iter().map(|p| V2 { x: p.0, y: p.1 }).collect() };
    let p = V2 { x, y };
    let Some((outer, holes)) = rings.split_first() else {
        return false;
    };
    point_in_ring(p, &ring(outer)) >= 0 && holes.iter().all(|h| point_in_ring(p, &ring(h)) != 1)
}

impl Space {
    fn el(&self, cat: &str, id: i64) -> Option<&El> {
        self.els.iter().find(|e| e.cat == cat && e.id == id)
    }

    /// The query box of `{ref}` or `{point}` (a point is a zero-size box), and the element
    /// to leave out of the answer.
    fn target(&self, v: &J) -> Result<(([f64; 3], [f64; 3]), Option<(&'static str, i64)>), String> {
        if let Some(s) = v["ref"].as_str() {
            let (cat, id) = parse_ref(s)?;
            let e = self
                .el(cat, id)
                .ok_or_else(|| {
                    let what = format!("no element with bounds: {}", fmt::text(s));
                    CmdError::wire(ErrorCode::NotFound, what)
                })?;
            return Ok(((e.lo, e.hi), Some((e.cat, e.id))));
        }
        if v["point"].is_array() {
            let p = vec3(&v["point"], "point")?;
            return Ok(((p, p), None));
        }
        if v["box"].is_object() {
            return Ok((box_of(&v["box"], "box")?, None));
        }
        Err("give ref, point or box".to_string())
    }
}

/// Runs the one query in `a`. `a.limit` (default 20, at most 100) cuts the hits; `total` counts all.
pub fn run(sp: &Space, a: &J) -> Result<Found, String> {
    let given: Vec<&'static str> = MODES.iter().copied().filter(|m| !a[*m].is_null()).collect();
    let [mode] = given[..] else {
        return Err(format!("give exactly one of {}", MODES.join(", ")));
    };
    let cats: Option<Vec<String>> = a["cats"].as_array().map(|v| {
        v.iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect()
    });
    let want = |e: &El| cats.as_ref().map_or(true, |c| c.iter().any(|x| x == e.cat));
    let q = &a[mode];
    let mut hits: Vec<Hit> = vec![];
    let mut push = |e: &El, d: f64| {
        hits.push(Hit {
            cat: e.cat,
            id: e.id,
            d,
        })
    };
    match mode {
        "near" => {
            let radius = q["radius"]
                .as_f64()
                .filter(|r| r.is_finite() && *r >= 0.0)
                .ok_or("near.radius must be a number of metres, 0 or more")?;
            let ((lo, hi), me) = sp.target(q)?;
            for e in sp.els.iter().filter(|e| want(e) && me != Some((e.cat, e.id))) {
                let d = gap(lo, hi, e.lo, e.hi);
                if d <= radius {
                    push(e, d);
                }
            }
        }
        "inside" => {
            if let Some(s) = q["room"].as_str() {
                let (cat, id) = parse_ref(s)?;
                let room = (cat == "room")
                    .then(|| sp.rooms.iter().find(|r| r.id == id))
                    .flatten()
                    .ok_or_else(|| format!("no room with a boundary: {}", fmt::text(s)))?;
                // the centre of the bounds must lie in the polygon, on the room's level
                for e in sp.els.iter().filter(|e| want(e) && e.level == room.level) {
                    let (cx, cy) = ((e.lo[0] + e.hi[0]) / 2.0, (e.lo[1] + e.hi[1]) / 2.0);
                    if inside_ring(&room.rings, cx, cy) {
                        push(e, 0.0);
                    }
                }
            } else {
                let (lo, hi) = box_of(&q["box"], "box")?;
                for e in sp.els.iter().filter(|e| want(e)) {
                    if (0..3).all(|i| e.lo[i] >= lo[i] && e.hi[i] <= hi[i]) {
                        push(e, 0.0);
                    }
                }
            }
        }
        "intersects" => {
            let ((lo, hi), me) = sp.target(q)?;
            for e in sp.els.iter().filter(|e| want(e) && me != Some((e.cat, e.id))) {
                if gap(lo, hi, e.lo, e.hi) == 0.0 {
                    push(e, 0.0);
                }
            }
        }
        "on_level" => {
            let level = q.as_i64().ok_or("on_level must be a level id")?;
            for e in sp.els.iter().filter(|e| want(e) && e.level == level) {
                push(e, 0.0);
            }
        }
        _ => {
            let o = vec3(&q["origin"], "ray.origin")?;
            let d = vec3(&q["dir"], "ray.dir")?;
            let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            if n < 1e-9 {
                return Err("ray.dir must not be zero".to_string());
            }
            let d = [d[0] / n, d[1] / n, d[2] / n];
            let tmax = q["max"]
                .as_f64()
                .filter(|m| m.is_finite() && *m > 0.0)
                .unwrap_or(DEFAULT_RAY_MAX);
            for e in sp.els.iter().filter(|e| want(e)) {
                if let Some(t) = ray_t(o, d, e.lo, e.hi, tmax) {
                    push(e, t);
                }
            }
        }
    }
    hits.sort_by(|x, y| {
        x.d.total_cmp(&y.d)
            .then(x.id.cmp(&y.id))
            .then(x.cat.cmp(y.cat))
    });
    let limit = a["limit"]
        .as_u64()
        .map_or(DEFAULT_LIMIT, |n| (n as usize).clamp(1, MAX_LIMIT));
    let total = hits.len();
    hits.truncate(limit);
    Ok(Found { mode, total, hits })
}

/// The reply a model reads: units first, then counts, then the rows.
pub fn answer(sp: &Space, a: &J, max_chars: usize) -> Result<Reply, String> {
    let f = run(sp, a)?;
    let with_d = matches!(f.mode, "near" | "ray");
    let rows: Vec<J> = f
        .hits
        .iter()
        .map(|h| {
            if with_d {
                json!({"ref": format!("{}:{}", h.cat, h.id), "dist": fmt::num(h.d)})
            } else {
                json!({"ref": format!("{}:{}", h.cat, h.id)})
            }
        })
        .collect();
    let mut o = fmt::Out::new(max_chars);
    o.put("units", &json!({"length": "m"}));
    o.put("mode", &json!(f.mode));
    o.put("total", &json!(f.total));
    o.put("returned", &json!(rows.len()));
    o.put("more", &json!(f.total - rows.len()));
    o.list("hits", &rows);
    Ok(o.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(cat: &'static str, id: i64, level: i64, lo: [f64; 3], hi: [f64; 3]) -> El {
        El { cat, id, level, lo, hi }
    }

    fn cube(cat: &'static str, id: i64, x: f64) -> El {
        el(cat, id, 1, [x, 0.0, 0.0], [x + 1.0, 1.0, 1.0])
    }

    fn ids(f: &Found) -> Vec<i64> {
        f.hits.iter().map(|h| h.id).collect()
    }

    #[test]
    fn near_sorted_by_distance() {
        let sp = Space {
            els: vec![cube("wall", 1, 10.0), cube("wall", 2, 3.0), cube("column", 3, 5.0)],
            rooms: vec![],
        };
        let f = run(&sp, &json!({"near": {"point": [0.0, 0.5, 0.5], "radius": 6.0}})).unwrap();
        assert_eq!(ids(&f), vec![2, 3]);
        assert!((f.hits[0].d - 3.0).abs() < 1e-9 && (f.hits[1].d - 5.0).abs() < 1e-9);
        // a ref leaves itself out; the gap is box to box
        let f = run(&sp, &json!({"near": {"ref": "wall:2", "radius": 2.5}})).unwrap();
        assert_eq!(ids(&f), vec![3]);
    }

    #[test]
    fn inside_room_uses_polygon_not_box() {
        // L-shaped room: the box of the polygon is 4 x 4, the notch is x>2, y>2
        let l = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 2.0), (2.0, 2.0), (2.0, 4.0), (0.0, 4.0)];
        let sp = Space {
            els: vec![
                el("column", 1, 1, [0.9, 0.9, 0.0], [1.1, 1.1, 3.0]),
                el("column", 2, 1, [2.9, 2.9, 0.0], [3.1, 3.1, 3.0]),
                el("column", 3, 2, [0.9, 0.9, 0.0], [1.1, 1.1, 3.0]),
            ],
            rooms: vec![Room { id: 7, level: 1, rings: vec![l] }],
        };
        let f = run(&sp, &json!({"inside": {"room": "room:7"}})).unwrap();
        assert_eq!(ids(&f), vec![1]);
        let f = run(&sp, &json!({"inside": {"box": {"min": [0, 0, 0], "max": [4, 4, 4]}}})).unwrap();
        assert_eq!(ids(&f), vec![1, 2, 3]);
    }

    #[test]
    fn ray_hits_first_element() {
        let sp = Space {
            els: vec![cube("wall", 5, 8.0), cube("wall", 6, 2.0), cube("wall", 7, -4.0)],
            rooms: vec![],
        };
        let f = run(
            &sp,
            &json!({"ray": {"origin": [0.0, 0.5, 0.5], "dir": [2.0, 0.0, 0.0]}, "limit": 1}),
        )
        .unwrap();
        assert_eq!(ids(&f), vec![6]);
        assert!((f.hits[0].d - 2.0).abs() < 1e-9);
        assert_eq!(f.total, 2);
    }

    #[test]
    fn intersects_and_on_level() {
        let sp = Space {
            els: vec![cube("wall", 1, 0.0), cube("wall", 2, 0.5), cube("beam", 3, 1.0), cube("beam", 4, 5.0)],
            rooms: vec![],
        };
        let f = run(&sp, &json!({"intersects": {"ref": "wall:1"}})).unwrap();
        assert_eq!(ids(&f), vec![2, 3]);
        let f = run(&sp, &json!({"on_level": 1, "cats": ["beam"]})).unwrap();
        assert_eq!(ids(&f), vec![3, 4]);
    }

    #[test]
    fn order_is_deterministic() {
        // equal distances: the lower id first, whatever the input order
        let a = Space {
            els: vec![cube("wall", 9, 3.0), cube("wall", 4, 3.0), cube("beam", 6, 3.0)],
            rooms: vec![],
        };
        let mut b = a.clone();
        b.els.reverse();
        let q = json!({"near": {"point": [0.0, 0.0, 0.0], "radius": 10.0}});
        assert_eq!(run(&a, &q).unwrap(), run(&b, &q).unwrap());
        assert_eq!(ids(&run(&a, &q).unwrap()), vec![4, 6, 9]);
    }

    #[test]
    fn limit_and_more_count() {
        let sp = Space {
            els: (1..=30).map(|i| cube("wall", i, i as f64 * 2.0)).collect(),
            rooms: vec![],
        };
        let q = json!({"near": {"point": [0.0, 0.0, 0.0], "radius": 1000.0}, "limit": 5});
        let f = run(&sp, &q).unwrap();
        assert_eq!((f.total, f.hits.len()), (30, 5));
        let r = answer(&sp, &q, 4000).unwrap();
        let v: J = serde_json::from_str(&r.text).unwrap();
        assert_eq!(v["returned"], 5);
        assert_eq!(v["more"], 25);
        assert_eq!(v["units"]["length"], "m");
        assert_eq!(v["hits"][0]["ref"], "wall:1");
        // the default limit applies without the argument
        let f = run(&sp, &json!({"on_level": 1})).unwrap();
        assert_eq!(f.hits.len(), 20);
    }

    #[test]
    fn empty_model() {
        let ed = Editor::new_in_memory().unwrap();
        let sp = space_of(&ed).unwrap();
        let r = answer(&sp, &json!({"near": {"point": [0, 0, 0], "radius": 5}}), 2000).unwrap();
        let v: J = serde_json::from_str(&r.text).unwrap();
        assert_eq!((v["total"].as_u64(), v["more"].as_u64()), (Some(0), Some(0)));
        assert!(v["hits"].as_array().unwrap().is_empty());
        assert!(run(&sp, &json!({})).is_err());
        assert!(run(&sp, &json!({"near": {"ref": "wall:1", "radius": 1}})).is_err());
    }
}
