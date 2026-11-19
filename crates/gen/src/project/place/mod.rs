//! Free placement. A straight spine corridor with rooms on both sides; its two ends may be
//! closed by full-depth end rooms, so the corridor runs only between the outermost doors.
//! Fixed parts (the core, pinned rooms) are placed first and the free runs of each side lie
//! between them. An exact branch-and-bound assigns the rooms to the end places and the runs;
//! a neighbourhood rebuild (LNS) then lowers what is left of the cost. Rooms keep their asked
//! area exactly: a run's length is the sum of its rooms' frontages, small rooms without a
//! window sit in the front strip of a host, and what a run does not use becomes a spare room
//! or grows a neighbour. Pure and deterministic: the same problem gives the same plan.

mod corridor;
mod flat;
mod lns;
mod search;

pub(crate) use corridor::{lay, Part, Rect};
pub(crate) use flat::{flat_cap, flat_width, interior, interior_around, Flat, FlatRoom};

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Mutex;
use web_time::{Duration, Instant};

/// Solved zones (floors, apartments) by their problem: a rebuild after an edit searches again
/// only the zones whose problem changed. The search is deterministic, so a kept plan is the
/// plan the search would find again.
static SOLVED: Mutex<Option<HashMap<String, Option<Plan>>>> = Mutex::new(None);
/// Plans kept at most; past it the store starts over.
const SOLVED_MAX: usize = 8192;

thread_local! {
    /// Zones this thread took from the store, and searched.
    static COUNTS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    /// When the running zone search stops, whatever its node budget.
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The longest zone search of this thread, ms.
    static SLOWEST: Cell<f64> = const { Cell::new(0.0) };
}

/// The longest zone search of this thread since the last call, ms; starts the next count.
pub(crate) fn slowest() -> f64 {
    SLOWEST.with(|s| s.replace(0.0))
}

/// Whether the running zone search is past its time cap.
pub(crate) fn late() -> bool {
    DEADLINE.with(|d| d.get().is_some_and(|t| Instant::now() >= t))
}

/// The plan of `p`: kept if the same problem was solved before, else searched.
pub(crate) fn solve(p: &Problem) -> Option<Plan> {
    let key = format!("{p:?}");
    let kept = SOLVED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&key).cloned());
    if let Some(plan) = kept {
        COUNTS.with(|c| c.set((c.get().0 + 1, c.get().1)));
        return plan;
    }
    let t = Instant::now();
    let cap = (p.ms > 0.0).then(|| t + Duration::from_secs_f64(p.ms / 1000.0));
    let was = DEADLINE.with(|d| d.replace(cap));
    let plan = search::solve(p);
    DEADLINE.with(|d| d.set(was));
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    SLOWEST.with(|s| s.set(s.get().max(ms)));
    COUNTS.with(|c| c.set((c.get().0, c.get().1 + 1)));
    let mut g = SOLVED.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if m.len() >= SOLVED_MAX {
        m.clear();
    }
    m.insert(key, plan.clone());
    plan
}

/// Zones this thread took from the store and zones it searched, since it started.
pub(crate) fn counts() -> (usize, usize) {
    COUNTS.with(Cell::get)
}

/// Forgets every kept plan.
pub(crate) fn forget() {
    *SOLVED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Narrowest frontage that takes a door and its clearances on both sides.
pub(crate) const DOOR_RUN: f64 = 1.2;
/// Depth a host keeps behind a front strip of small rooms.
pub(crate) const BACK_MIN: f64 = 1.5;
/// Shallowest front strip.
pub(crate) const STRIP_MIN: f64 = 1.0;
pub(crate) const EPS: f64 = 1e-6;
/// Cost of one square metre of each part of the objective, from the `layout.cost_*` rules:
/// corridor, a room grown beyond its asked area, a circulation room (hall, lobby) grown, a
/// small store, a spare room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Costs {
    pub corridor: f64,
    pub grow: f64,
    pub grow_circ: f64,
    pub store: f64,
    pub spare: f64,
}

impl Costs {
    /// The weights of the active rule set.
    pub(crate) fn ruled() -> Costs {
        use super::rules::rule;
        Costs {
            corridor: rule("layout.cost_corridor"),
            grow: rule("layout.cost_grow"),
            grow_circ: rule("layout.cost_grow_circulation"),
            store: rule("layout.cost_store"),
            spare: rule("layout.cost_spare"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Side {
    South,
    North,
}

impl Side {
    pub(crate) fn at(self) -> usize {
        match self {
            Side::South => 0,
            Side::North => 1,
        }
    }
}

/// One room to place.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    /// The caller's index of the room.
    pub unit: usize,
    pub area: f64,
    /// Frontage kept whatever the depth (the entrance); its area follows the depth.
    pub width: Option<f64>,
    /// No window needed: it may sit in the front strip of a host.
    pub inner: bool,
    /// It may take leftover length.
    pub grows: bool,
    /// It may close an end of the spine, `end_w` wide (else its area over the full depth).
    pub end: bool,
    pub end_w: Option<f64>,
    /// The sides it may stand on, south and north.
    pub sides: [bool; 2],
    /// A circulation room (lobby, hall): leftover it takes is circulation, costed as such.
    pub circ: bool,
}

impl Item {
    /// A room of the given area that may stand anywhere and take leftover length.
    pub(crate) fn room(unit: usize, area: f64, inner: bool) -> Item {
        Item { unit, area, width: None, inner, grows: true, end: true, end_w: None, sides: [true, true], circ: false }
    }

    /// The same item apart from the caller's index.
    fn same(&self, o: &Item) -> bool {
        Item { unit: o.unit, ..self.clone() } == *o
    }
}

/// A stretch of a side already taken: core cells, a pinned room.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Fixed {
    pub side: Side,
    pub x0: f64,
    pub x1: f64,
}

/// What stands at one end of the spine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Cap {
    /// The spine runs to the facade.
    Open,
    /// An item takes the full depth.
    Item(usize),
    /// A pinned room of this width takes the full depth.
    Fixed(f64),
}

/// A floor to place: footprint `w` along the spine, the side depths, the spine width.
#[derive(Clone, Debug)]
pub(crate) struct Problem {
    pub w: f64,
    /// South and north depth, the spine between them.
    pub depth: [f64; 2],
    pub corridor: f64,
    /// Narrowest room with a window.
    pub min_w: f64,
    pub items: Vec<Item>,
    pub fixed: Vec<Fixed>,
    /// Pinned end rooms, west and east; `None` lets the search choose.
    pub caps: [Option<Cap>; 2],
    /// Branch-and-bound nodes, then neighbourhood rounds.
    pub nodes: usize,
    pub rounds: usize,
    /// A side with no facade (inside an apartment, the strip on the public corridor): only
    /// rooms without a window stand there, and what is left over is a store.
    pub blind: [bool; 2],
    pub costs: Costs,
    /// Wall-time cap of the search and rebuild in ms (`layout.search_ms`), 0 for none.
    pub ms: f64,
}

/// A free stretch of one side and the items placed in it, in order along x.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Run {
    pub side: Side,
    pub x0: f64,
    pub x1: f64,
    pub items: Vec<usize>,
}

impl Run {
    pub(crate) fn len(&self) -> f64 {
        self.x1 - self.x0
    }
}

/// A placed floor.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Plan {
    pub caps: [Cap; 2],
    pub runs: Vec<Run>,
    /// The spine, `x` from and to.
    pub spine: (f64, f64),
    pub cost: f64,
}

impl Problem {
    /// Full depth of the plate: both sides and the spine.
    pub(crate) fn full(&self) -> f64 {
        self.depth[0] + self.corridor + self.depth[1]
    }

    /// Cost of a spine `len` long.
    pub(crate) fn spine_cost(&self, len: f64) -> f64 {
        self.costs.corridor * self.corridor * len
    }

    /// Frontage of item `i` on `side`.
    pub(crate) fn len(&self, i: usize, side: Side) -> f64 {
        let it = &self.items[i];
        it.width.unwrap_or(it.area / self.depth[side.at()])
    }

    /// Frontage of item `i` as an end room.
    pub(crate) fn cap_len(&self, i: usize) -> f64 {
        let it = &self.items[i];
        it.end_w.or(it.width).unwrap_or(it.area / self.full())
    }

    /// Whether item `i` goes into the front strip of a host on `side`.
    pub(crate) fn tucked(&self, i: usize, side: Side) -> bool {
        let it = &self.items[i];
        it.inner && it.width.is_none() && self.len(i, side) < self.min_w - EPS
    }

    /// Whether item `i` may stand on `side` at all, alone or in a front strip.
    pub(crate) fn allowed(&self, i: usize, side: Side) -> bool {
        let s = side.at();
        if self.depth[s] < EPS || !self.items[i].sides[s] || (self.blind[s] && !self.items[i].inner) {
            return false;
        }
        let l = self.len(i, side);
        if self.tucked(i, side) {
            // a strip deep enough for the door frontage, shallow enough to leave the host its back
            self.items[i].area / DOOR_RUN >= STRIP_MIN - EPS && self.depth[side.at()] - BACK_MIN >= STRIP_MIN - EPS
        } else {
            l >= self.min_w - EPS || (self.items[i].inner && l >= DOOR_RUN - EPS)
        }
    }

    /// Whether item `i` may close an end of the spine.
    pub(crate) fn cap_ok(&self, i: usize) -> bool {
        self.items[i].end && self.cap_len(i) >= self.min_w - EPS
    }

    /// The width taken at each end.
    pub(crate) fn cap_w(&self, c: Cap) -> f64 {
        match c {
            Cap::Open => 0.0,
            Cap::Item(i) => self.cap_len(i),
            Cap::Fixed(w) => w,
        }
    }

    /// The free runs between the end rooms and the fixed parts; `None` when a fixed part
    /// sticks out of the spine.
    pub(crate) fn runs(&self, caps: [Cap; 2]) -> Option<(f64, f64, Vec<Run>)> {
        let (xa, xb) = (self.cap_w(caps[0]), self.w - self.cap_w(caps[1]));
        if xb - xa < DOOR_RUN - EPS {
            return None;
        }
        let mut runs = vec![];
        for side in [Side::South, Side::North] {
            let mut fx: Vec<Fixed> = self.fixed.iter().copied().filter(|f| f.side == side).collect();
            // a side without depth (the gallery's open side) has no runs and holds nothing
            if self.depth[side.at()] < EPS {
                if !fx.is_empty() {
                    return None;
                }
                continue;
            }
            fx.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            let mut x = xa;
            for f in fx {
                if f.x0 < xa - EPS || f.x1 > xb + EPS || f.x0 < x - EPS {
                    return None;
                }
                if f.x0 - x > EPS {
                    runs.push(Run { side, x0: x, x1: f.x0, items: vec![] });
                }
                x = f.x1;
            }
            if xb - x > EPS {
                runs.push(Run { side, x0: x, x1: xb, items: vec![] });
            }
        }
        Some((xa, xb, runs))
    }
}

/// Tuck groups of a run: for each host (an index into `run.items`), the items in its front
/// strip and the strip depth. `None` when a small room finds no host.
pub(crate) fn hosts(p: &Problem, run: &Run) -> Option<Vec<(usize, Vec<usize>, f64)>> {
    let depth = p.depth[run.side.at()];
    let mut tucks: Vec<usize> = run.items.iter().copied().filter(|&i| p.tucked(i, run.side)).collect();
    if tucks.is_empty() {
        return Some(vec![]);
    }
    tucks.sort_by(|a, b| p.items[*b].area.total_cmp(&p.items[*a].area).then(a.cmp(b)));
    let mut groups: Vec<(usize, Vec<usize>, f64)> = run
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| !p.tucked(**i, run.side) && p.items[**i].width.is_none())
        .map(|(k, _)| (k, vec![], 0.0))
        .collect();
    for t in tucks {
        let fit = groups.iter().position(|g| strip(p, run.items[g.0], &g.1, t, depth).is_some());
        let k = fit?;
        groups[k].1.push(t);
        groups[k].2 = strip(p, run.items[groups[k].0], &groups[k].1, usize::MAX, depth)?;
    }
    groups.retain(|g| !g.1.is_empty());
    Some(groups)
}

/// The strip depth of host `h` with `group` and `more` (`usize::MAX` for none), when the host
/// keeps a door frontage.
fn strip(p: &Problem, h: usize, group: &[usize], more: usize, depth: f64) -> Option<f64> {
    let all: Vec<usize> = group.iter().copied().chain((more != usize::MAX).then_some(more)).collect();
    let small = all.iter().map(|&t| p.items[t].area).fold(f64::MAX, f64::min);
    let dt = (depth - BACK_MIN).min(small / DOOR_RUN);
    if dt < STRIP_MIN - EPS {
        return None;
    }
    let tucked: f64 = all.iter().map(|&t| p.items[t].area).sum();
    let slot = (p.items[h].area + tucked) / depth;
    let full = slot - tucked / dt;
    (full >= DOOR_RUN - EPS).then_some(dt)
}

/// What a run leaves over and what that costs: `None` when it does not hold its items.
pub(crate) fn run_cost(p: &Problem, run: &Run) -> Option<f64> {
    let used: f64 = run.items.iter().map(|&i| p.len(i, run.side)).sum();
    let left = run.len() - used;
    if left < -EPS {
        return None;
    }
    hosts(p, run)?;
    let depth = p.depth[run.side.at()];
    if left <= EPS {
        return Some(0.0);
    }
    let c = &p.costs;
    if left >= p.min_w - EPS && !p.blind[run.side.at()] {
        return Some(c.spare * left * depth);
    }
    if stores_left(p, run, left) {
        return Some(c.store * left * depth);
    }
    if let Some(g) = grower(p, run) {
        return Some(if p.items[g].circ { c.grow_circ } else { c.grow } * left * depth);
    }
    (left >= DOOR_RUN - EPS).then_some(c.store * left * depth)
}

/// The item of a run that takes a short leftover: the last one that may grow, a room before a
/// circulation room.
pub(crate) fn grower(p: &Problem, run: &Run) -> Option<usize> {
    let can = |i: usize| p.items[i].grows && p.items[i].width.is_none() && !p.tucked(i, run.side);
    let last = |circ: bool| run.items.iter().rev().copied().find(|&i| can(i) && p.items[i].circ == circ);
    last(false).or_else(|| last(true))
}

/// Whether a run's short leftover becomes a store rather than grows a circulation room into
/// more circulation.
pub(crate) fn stores_left(p: &Problem, run: &Run, left: f64) -> bool {
    let c = &p.costs;
    left >= DOOR_RUN - EPS && grower(p, run).is_some_and(|g| p.items[g].circ && c.store < c.grow_circ)
}

/// Splitmix64: the neighbourhood rebuild's deterministic choices.
pub(crate) fn mix(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
