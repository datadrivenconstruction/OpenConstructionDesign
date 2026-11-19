// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Stable element keys of a built project: a semantic path per element, derived from what the
//! element is (storey, apartment, room kind, the rooms a wall separates) and never from a
//! vector index or a coordinate that moves with a neighbour. The same spec gives the same keys;
//! a change in one place leaves the keys elsewhere alone. Grammar of the contract: `^[A-Za-z0-9_$][A-Za-z0-9._#/:$-]*$`.

use super::{GProject, PRole, PStorey};
use crate::{GLayout, WallRole};
use std::collections::{BTreeMap, BTreeSet};

/// Version of the key scheme; written beside the keys so a later scheme can be told apart.
pub const KEY_SCHEME: u32 = 1;

/// Keys of one storey, in the order of the layout vectors.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoreyKeys {
    pub walls: Vec<String>,
    pub openings: Vec<String>,
    pub rooms: Vec<String>,
    pub slab: Option<String>,
    pub stairs: Vec<String>,
    pub shafts: Vec<String>,
    pub grids: Vec<String>,
    pub columns: Vec<String>,
    pub fixtures: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectKeys {
    pub storeys: Vec<StoreyKeys>,
    pub roof: String,
}

impl ProjectKeys {
    /// Every key once, storeys in order, then the roof.
    pub fn all(&self) -> Vec<&str> {
        let mut out = vec![];
        for s in &self.storeys {
            let groups = [&s.walls, &s.openings, &s.rooms, &s.stairs, &s.shafts, &s.grids];
            for g in groups.into_iter().chain([&s.columns, &s.fixtures]) {
                out.extend(g.iter().map(String::as_str));
            }
            out.extend(s.slab.as_deref());
        }
        out.push(&self.roof);
        out
    }
}

/// Lower-case ASCII with `-` between runs of anything else; a non-ASCII letter becomes its
/// code point so two different names never share a slug.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if c.is_ascii() {
            if !out.ends_with('-') && !out.is_empty() {
                out.push('-');
            }
        } else {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
            out.push_str(&format!("u{:x}", c as u32));
        }
    }
    let t = out.trim_end_matches('-').to_string();
    if t.is_empty() {
        "x".into()
    } else {
        t
    }
}

/// `base` for the first use, `base#2`, `base#3` for the next.
#[derive(Default)]
struct Count(BTreeMap<String, u32>);

impl Count {
    fn next(&mut self, base: &str) -> String {
        let n = self.0.entry(base.to_string()).or_insert(0);
        *n += 1;
        if *n == 1 {
            base.to_string()
        } else {
            format!("{base}#{n}")
        }
    }
}

fn inside(poly: &[(f64, f64)], p: (f64, f64)) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
    }
    c
}

fn room_at(l: &GLayout, p: (f64, f64)) -> Option<usize> {
    l.rooms.iter().position(|r| inside(&r.polygon, p))
}

fn facade(n: (f64, f64)) -> &'static str {
    if n.0.abs() > n.1.abs() {
        if n.0 > 0.0 {
            "E"
        } else {
            "W"
        }
    } else if n.1 > 0.0 {
        "N"
    } else {
        "S"
    }
}

/// Probe distance from a wall centre line: inside the neighbouring room, clear of its face.
const PROBE: f64 = 0.06;

/// Room labels of one storey: `u02/room-kitchen` in apartment 2 of the floor, `p/room-hall`
/// among the shared rooms; a second room of a kind in the same place is `...#2`.
fn room_labels(s: &PStorey) -> Vec<String> {
    let l = &s.layout;
    let mut apts: Vec<u32> = s
        .rooms
        .iter()
        .filter_map(|r| match r.owner {
            PRole::Apartment(a) => Some(a),
            PRole::Public => None,
        })
        .collect();
    apts.sort_unstable();
    apts.dedup();
    let mut count = Count::default();
    (0..l.rooms.len())
        .map(|i| {
            let kind = s
                .rooms
                .get(i)
                .map_or_else(|| slug(&l.rooms[i].name), |r| format!("{:?}", r.kind).to_lowercase());
            let owner = match s.rooms.get(i).map(|r| r.owner) {
                Some(PRole::Apartment(a)) => {
                    let rank = apts.iter().position(|x| *x == a).unwrap_or(0) + 1;
                    format!("u{rank:02}")
                }
                _ => "p".to_string(),
            };
            count.next(&format!("{owner}/room-{kind}"))
        })
        .collect()
}

fn role_slug(r: WallRole) -> String {
    format!("{r:?}").to_lowercase()
}

fn wall_keys(l: &GLayout, rooms: &[String]) -> Vec<String> {
    let side = |p: (f64, f64)| room_at(l, p);
    let mut base: Vec<(String, (i64, i64))> = Vec::with_capacity(l.walls.len());
    for w in &l.walls {
        let mid = ((w.a.0 + w.b.0) / 2.0, (w.a.1 + w.b.1) / 2.0);
        let (dx, dy) = (w.b.0 - w.a.0, w.b.1 - w.a.1);
        let len = (dx * dx + dy * dy).sqrt().max(1e-9);
        let n = (-dy / len, dx / len);
        let plus = side((mid.0 + n.0 * PROBE, mid.1 + n.1 * PROBE));
        let minus = side((mid.0 - n.0 * PROBE, mid.1 - n.1 * PROBE));
        let tag = |r: usize| rooms[r].replace('/', ".");
        let label = match (plus, minus) {
            (Some(a), Some(b)) => {
                let (mut x, mut y) = (tag(a), tag(b));
                if x > y {
                    std::mem::swap(&mut x, &mut y);
                }
                format!("{x}:{y}")
            }
            (Some(a), None) => format!("{}:ext-{}", tag(a), facade((-n.0, -n.1))),
            (None, Some(b)) => format!("{}:ext-{}", tag(b), facade(n)),
            (None, None) => "free".to_string(),
        };
        let at = (
            (w.a.0.min(w.b.0) * 1000.0).round() as i64,
            (w.a.1.min(w.b.1) * 1000.0).round() as i64,
        );
        base.push((format!("wall-{}/{label}", role_slug(w.role)), at));
    }
    // pieces of one kind between the same rooms are numbered along the plan
    let mut by: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, (k, _)) in base.iter().enumerate() {
        by.entry(k.as_str()).or_default().push(i);
    }
    let mut out = vec![String::new(); base.len()];
    for (k, mut idx) in by {
        idx.sort_by_key(|&i| (base[i].1, i));
        for (n, i) in idx.into_iter().enumerate() {
            out[i] = format!("{k}/{n}");
        }
    }
    out
}

fn opening_keys(l: &GLayout, walls: &[String]) -> Vec<String> {
    let mut out = vec![String::new(); l.openings.len()];
    let mut by: BTreeMap<(usize, bool), Vec<usize>> = BTreeMap::new();
    for (i, o) in l.openings.iter().enumerate() {
        by.entry((o.wall, o.door)).or_default().push(i);
    }
    for ((w, door), mut idx) in by {
        idx.sort_by(|&a, &b| {
            l.openings[a]
                .along
                .total_cmp(&l.openings[b].along)
                .then(a.cmp(&b))
        });
        for (n, i) in idx.into_iter().enumerate() {
            let what = if door { "door" } else { "window" };
            out[i] = format!("{}/{what}-{}", walls[w], n + 1);
        }
    }
    out
}

/// Grid lines by direction and position along the plan, so the key does not follow the label
/// (which differs by drawing language).
fn grid_keys(l: &GLayout) -> Vec<String> {
    let mut out = vec![String::new(); l.grids.len()];
    for vertical in [true, false] {
        let mut idx: Vec<usize> = (0..l.grids.len())
            .filter(|&i| {
                let g = &l.grids[i];
                ((g.b.0 - g.a.0).abs() < (g.b.1 - g.a.1).abs()) == vertical
            })
            .collect();
        let pos = |i: usize| {
            let g = &l.grids[i];
            if vertical {
                g.a.0
            } else {
                g.a.1
            }
        };
        idx.sort_by(|&a, &b| pos(a).total_cmp(&pos(b)).then(a.cmp(&b)));
        for (n, i) in idx.into_iter().enumerate() {
            out[i] = format!("grid-{}{:02}", if vertical { "v" } else { "h" }, n + 1);
        }
    }
    out
}

/// Columns by their place in the column grid (column and row number, counted from the origin),
/// so a wider footprint adds columns at the far end and leaves the near ones alone.
fn column_keys(l: &GLayout) -> Vec<String> {
    let cm = |v: f64| (v * 100.0).round() as i64;
    let rank = |vals: Vec<i64>, v: i64| {
        let mut s = vals;
        s.sort_unstable();
        s.dedup();
        s.iter().position(|x| *x == v).unwrap_or(0) + 1
    };
    let xs: Vec<i64> = l.columns.iter().map(|c| cm(c.at.0)).collect();
    let ys: Vec<i64> = l.columns.iter().map(|c| cm(c.at.1)).collect();
    l.columns
        .iter()
        .map(|c| {
            let (cx, cy) = (rank(xs.clone(), cm(c.at.0)), rank(ys.clone(), cm(c.at.1)));
            format!("col-c{cx:02}r{cy:02}")
        })
        .collect()
}

fn storey_keys(s: &PStorey, pre: &str) -> StoreyKeys {
    let l = &s.layout;
    let labels = room_labels(s);
    let walls = wall_keys(l, &labels);
    let openings = opening_keys(l, &walls);
    let full = |rel: &str| format!("{pre}/{rel}");
    let mut fx = Count::default();
    let fixtures = l
        .fixtures
        .iter()
        .map(|f| {
            let room = room_at(l, f.at)
                .or_else(|| f.wall.and_then(|w| wall_room(l, w)))
                .map_or_else(|| "free".to_string(), |r| labels[r].clone());
            let item = format!("fx-{}-{}", slug(f.family), slug(f.ty));
            fx.next(&format!("{}/{room}/{item}", pre))
        })
        .collect();
    let mut stairs = Count::default();
    let mut shafts = Count::default();
    let mut cols = Count::default();
    StoreyKeys {
        walls: walls.iter().map(|k| full(k)).collect(),
        openings: openings.iter().map(|k| full(k)).collect(),
        rooms: labels.iter().map(|k| full(k)).collect(),
        slab: (l.slab.len() >= 3).then(|| full("slab")),
        stairs: l.stairs.iter().map(|_| stairs.next(&full("stair"))).collect(),
        shafts: l.shafts.iter().map(|_| shafts.next(&full("shaft"))).collect(),
        grids: grid_keys(l).iter().map(|k| full(k)).collect(),
        columns: column_keys(l).iter().map(|k| cols.next(&full(k))).collect(),
        fixtures,
    }
}

/// The room on the face side of a wall-hosted fixture whose point fell outside every room.
fn wall_room(l: &GLayout, w: usize) -> Option<usize> {
    let wall = l.walls.get(w)?;
    let mid = ((wall.a.0 + wall.b.0) / 2.0, (wall.a.1 + wall.b.1) / 2.0);
    let (dx, dy) = (wall.b.0 - wall.a.0, wall.b.1 - wall.a.1);
    let len = (dx * dx + dy * dy).sqrt().max(1e-9);
    let n = (-dy / len, dx / len);
    room_at(l, (mid.0 + n.0 * PROBE, mid.1 + n.1 * PROBE))
        .or_else(|| room_at(l, (mid.0 - n.0 * PROBE, mid.1 - n.1 * PROBE)))
}

/// Keys for building number `building` (1-based): `b1/s03/u02/room-kitchen`,
/// `b1/s03/wall-party/u01.room-living+u02.room-living/0`.
pub fn project_keys(p: &GProject, building: u32) -> ProjectKeys {
    let storeys = p
        .storeys
        .iter()
        .enumerate()
        .map(|(k, s)| storey_keys(s, &format!("b{building}/s{:02}", k + 1)))
        .collect();
    let mut keys = ProjectKeys {
        storeys,
        roof: format!("b{building}/roof"),
    };
    make_unique(&mut keys);
    keys
}

/// A key shared by two elements gets `.dup2`, `.dup3` on the later ones, so the set is always unique.
fn make_unique(k: &mut ProjectKeys) {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut fix = |key: &mut String| {
        if seen.insert(key.clone()) {
            return;
        }
        let mut n = 2;
        loop {
            let t = format!("{key}.dup{n}");
            if seen.insert(t.clone()) {
                *key = t;
                return;
            }
            n += 1;
        }
    };
    for s in &mut k.storeys {
        for g in [
            &mut s.walls,
            &mut s.openings,
            &mut s.rooms,
            &mut s.stairs,
            &mut s.shafts,
            &mut s.grids,
            &mut s.columns,
            &mut s.fixtures,
        ] {
            g.iter_mut().for_each(&mut fix);
        }
        if let Some(x) = s.slab.as_mut() {
            fix(x);
        }
    }
    fix(&mut k.roof);
}
