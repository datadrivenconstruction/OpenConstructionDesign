//! An apartment as a zone of the floor, its rooms placed inside it by the same solver: the
//! apartment's hall stands in for the corridor. Local frame: `x` along the zone's edge on the
//! public corridor, `y` back from that edge. The strip at `y = 0` is blind (rooms without a
//! window and the entrance from the corridor), then the hall, then the facade rooms; the two
//! ends of the hall may be closed by rooms of the full depth. The caller maps the rectangles.

use super::{lay, solve, Fixed, Item, Part, Problem, Rect, Side, EPS};

/// Clear width of an apartment hall: a door at each end of it still has its clearances.
pub(crate) const HALL_W: f64 = 1.4;
/// Width of the entrance from the public corridor through the blind strip.
pub(crate) const VEST_W: f64 = 1.5;
/// Depths of the blind strip tried, shallowest first.
const STRIPS: [f64; 4] = [1.5, 1.8, 2.1, 2.4];
/// The search of one apartment is small: a handful of rooms.
const FLAT_NODES: usize = 20_000;
const FLAT_ROUNDS: usize = 40;
/// Widest zone tried, as a multiple of the narrowest that could hold the rooms' area.
const WIDEN_MAX: f64 = 3.0;

/// A room of an apartment: its area and whether it may do without a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FlatRoom {
    pub area: f64,
    pub inner: bool,
}

/// An apartment laid out: `Part::Item(i)` is room `i`, `Part::Spare(j)` store `j`,
/// `Part::Corridor` the hall (with its entrance).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Flat {
    pub rects: Vec<Rect>,
    pub stores: usize,
    /// Rooms that took leftover length.
    pub grown: Vec<usize>,
    pub cost: f64,
}

/// The rooms in a zone `w` long and `depth` deep whose entrance spans `door` along the blind
/// edge; `None` picks the cheapest of the ends and the middle. `None` when nothing holds them.
pub(crate) fn interior(rooms: &[FlatRoom], w: f64, depth: f64, door: Option<(f64, f64)>, min_w: f64) -> Option<Flat> {
    interior_around(rooms, w, depth, door, min_w, &[])
}

/// As [`interior`], with pinned rooms kept where they are: `held` gives a room's index and its
/// local rectangle, on the blind strip, behind the hall or across the full depth at an end.
/// The strip depth follows from them; the other rooms are laid out around.
pub(crate) fn interior_around(rooms: &[FlatRoom], w: f64, depth: f64, door: Option<(f64, f64)>, min_w: f64, held: &[(usize, Rect)]) -> Option<Flat> {
    let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
    let mut strips: Vec<f64> = STRIPS.to_vec();
    let mut caps = [None, None];
    let mut sides: Vec<(Side, f64, f64, f64, f64)> = vec![];
    for (_, r) in held {
        if near(r.y0, 0.0) && near(r.y1, depth) {
            if near(r.x0, 0.0) {
                caps[0] = Some(super::Cap::Fixed(r.x1));
            } else if near(r.x1, w) {
                caps[1] = Some(super::Cap::Fixed(w - r.x0));
            } else {
                return None;
            }
        } else if near(r.y0, 0.0) {
            strips = vec![r.y1];
            sides.push((Side::South, r.x0, r.x1, r.y0, r.y1));
        } else if near(r.y1, depth) {
            strips = vec![r.y0 - HALL_W];
            sides.push((Side::North, r.x0, r.x1, r.y0, r.y1));
        } else {
            return None;
        }
    }
    // every held room agrees on the strip depth
    for &(s, _, _, y0, y1) in &sides {
        let df = strips[0];
        if (s == Side::South && !near(y1, df)) || (s == Side::North && !near(y0, df + HALL_W)) {
            return None;
        }
    }
    let free: Vec<usize> = (0..rooms.len()).filter(|i| !held.iter().any(|h| h.0 == *i)).collect();
    let items: Vec<Item> = free.iter().map(|&i| Item::room(i, rooms[i].area, rooms[i].inner)).collect();
    let fixed0: Vec<Fixed> = sides.iter().map(|&(side, x0, x1, _, _)| Fixed { side, x0, x1 }).collect();
    let doors: Vec<(f64, f64)> = match door {
        Some(d) => vec![d],
        None if w < VEST_W - EPS => vec![],
        None => {
            let mut v = vec![(0.0, VEST_W), ((w - VEST_W) / 2.0, (w + VEST_W) / 2.0), (w - VEST_W, w)];
            v.dedup_by(|a, b| (a.0 - b.0).abs() < EPS);
            v
        }
    };
    let mut best: Option<Flat> = None;
    for df in strips {
        if depth - df - HALL_W < min_w - EPS {
            break;
        }
        for &(x0, x1) in &doors {
            let p = Problem {
                w,
                depth: [df, depth - df - HALL_W],
                corridor: HALL_W,
                min_w,
                items: items.clone(),
                fixed: fixed0.iter().copied().chain([Fixed { side: Side::South, x0, x1 }]).collect(),
                caps,
                nodes: FLAT_NODES,
                rounds: FLAT_ROUNDS,
                blind: [true, false],
                costs: super::Costs::ruled(),
                ms: super::super::rules::rule("layout.search_ms"),
            };
            let Some(plan) = solve(&p) else { continue };
            if best.as_ref().is_some_and(|b| b.cost <= plan.cost + EPS) {
                continue;
            }
            let shape = lay(&p, &plan);
            // item indices back to room indices, and the held rooms as they were
            let mut rects: Vec<Rect> = shape
                .rects
                .iter()
                .map(|r| match r.part {
                    Part::Item(j) => Rect { part: Part::Item(p.items[j].unit), ..*r },
                    _ => *r,
                })
                .collect();
            rects.extend(held.iter().map(|(i, r)| Rect { part: Part::Item(*i), ..*r }));
            rects.push(Rect { x0, y0: 0.0, x1, y1: df, part: Part::Corridor });
            let grown = shape.grown.iter().map(|&j| p.items[j].unit).collect();
            best = Some(Flat { rects, stores: shape.spares.len(), grown, cost: plan.cost });
        }
    }
    best
}

/// The narrowest zone on `module` steps that holds the rooms at `depth` behind the corridor.
pub(crate) fn flat_width(rooms: &[FlatRoom], depth: f64, min_w: f64, module: f64) -> Option<f64> {
    let area: f64 = rooms.iter().map(|r| r.area).sum();
    let snap = |x: f64| (x / module - 1e-9).ceil() * module;
    let lo = snap((area / depth).max(VEST_W));
    let mut w = lo;
    while w <= lo * WIDEN_MAX + EPS {
        if interior(rooms, w, depth, None, min_w).is_some() {
            return Some(w);
        }
        w = snap(w + module);
    }
    None
}

/// The narrowest end zone on `module` steps: the full depth `full` long, entered from the
/// corridor end at `door`.
pub(crate) fn flat_cap(rooms: &[FlatRoom], full: f64, door: (f64, f64), min_w: f64, module: f64) -> Option<f64> {
    let snap = |x: f64| (x / module - 1e-9).ceil() * module;
    let lo = snap(STRIPS[0] + HALL_W + min_w);
    let mut d = lo;
    while d <= full + EPS {
        if interior(rooms, full, d, Some(door), min_w).is_some() {
            return Some(d);
        }
        d = snap(d + module);
    }
    None
}
