// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Furniture and fixtures from the fit-out rules (`fit_rules`): every room takes the first rule
//! set whose needs it can meet. Pieces stand against the walls or on a grid, clear of door
//! swings, columns, one another and the free space each piece keeps in front and beside it;
//! tall pieces keep off the windows, radiators sit under them, wet pieces gather on the wet
//! side, and every drain is checked against its stack.

use super::fit_rules::{
    fixture_at, hit, inside, Count, Face, Org, Placed, Rules, Run, Set, Sides, Slot, Step, EPS, R,
};
use super::{PRole, RoomFit, RoomKind, Typology};
use crate::{GFixture, GLayout, WallRole};
use std::collections::HashSet;

const STEP: f64 = 0.05;
/// A run may leave this much wall uncovered where no filler fits (a filler strip).
const STRIP: f64 = 0.15;
/// Spacing of the spots along the wet side a stack is tried at.
const STACK_STEP: f64 = 0.25;

/// A room as the tiler made it: axis-aligned, on wall centre lines. A room of any outline
/// comes as its usable rectangle, with the door swings of the walls off that rectangle in
/// `keep`.
pub(crate) struct RoomIn<'a> {
    pub rect: R,
    pub kind: RoomKind,
    pub owner: PRole,
    pub name: &'a str,
    pub tags: super::RoomTags,
    pub keep: Vec<R>,
}

fn span(p: (f64, f64), q: (f64, f64)) -> R {
    [p.0.min(q.0), p.1.min(q.1), p.0.max(q.0), p.1.max(q.1)]
}

/// A side of the room's clear rectangle walked counter-clockwise; `n` points into the room.
#[derive(Clone, Copy)]
pub(crate) struct Side {
    o: (f64, f64),
    t: (f64, f64),
    pub len: f64,
    /// The wall axis behind the side: horizontal?, its line; exterior?
    pub horiz: bool,
    pub line: f64,
    pub ext: bool,
}

impl Side {
    fn n(&self) -> (f64, f64) {
        (-self.t.1, self.t.0)
    }

    pub fn at(&self, s: f64, d: f64) -> (f64, f64) {
        let n = self.n();
        (self.o.0 + self.t.0 * s + n.0 * d, self.o.1 + self.t.1 * s + n.1 * d)
    }

    pub fn band(&self, s0: f64, s1: f64, d0: f64, d1: f64) -> R {
        span(self.at(s0, d0), self.at(s1, d1))
    }

    fn s_of(&self, p: (f64, f64)) -> f64 {
        (p.0 - self.o.0) * self.t.0 + (p.1 - self.o.1) * self.t.1
    }

    /// A piece's front direction in the plan.
    fn front(&self, f: Face) -> (f64, f64) {
        let n = self.n();
        match f {
            Face::In => n,
            Face::Out => (-n.0, -n.1),
            Face::Left => (-self.t.0, -self.t.1),
            Face::Right => self.t,
        }
    }
}

/// The fixed geometry of a room: clear rectangle, sides, windows, door swings, wet side.
pub(crate) struct Geo<'a> {
    l: &'a GLayout,
    pub clear: R,
    pub sides: [Side; 4],
    pub windows: Vec<(usize, f64, f64)>,
    pub doors: Vec<R>,
    columns: Vec<R>,
    /// Sides by wet preference: the wet side, its neighbours, the opposite one.
    pub wet: [usize; 4],
}

fn wet_kind(k: RoomKind) -> bool {
    matches!(k, RoomKind::Bath | RoomKind::Wc | RoomKind::Kitchen)
}

impl<'a> Geo<'a> {
    pub fn new(l: &'a GLayout, me: &RoomIn<'_>, rooms: &[RoomIn<'_>], rules: &Rules) -> Geo<'a> {
        let [x0, y0, x1, y1] = me.rect;
        let half = |horiz: bool, line: f64, a: f64, b: f64| {
            let mid = (a + b) / 2.0;
            wall_at(l, horiz, line, mid, mid)
                .map(|k| (l.walls[k].role.thickness() / 2.0, l.walls[k].role == WallRole::Exterior))
                .unwrap_or((0.0, false))
        };
        let (ts, es) = half(true, y0, x0, x1);
        let (te, ee) = half(false, x1, y0, y1);
        let (tn, en) = half(true, y1, x0, x1);
        let (tw, ew) = half(false, x0, y0, y1);
        let clear = [x0 + tw, y0 + ts, x1 - te, y1 - tn];
        let (w, d) = ((clear[2] - clear[0]).max(0.0), (clear[3] - clear[1]).max(0.0));
        let side = |o, t, len, horiz, line, ext| Side { o, t, len, horiz, line, ext };
        let sides = [
            side((clear[0], clear[1]), (1.0, 0.0), w, true, y0, es),
            side((clear[2], clear[1]), (0.0, 1.0), d, false, x1, ee),
            side((clear[2], clear[3]), (-1.0, 0.0), w, true, y1, en),
            side((clear[0], clear[3]), (0.0, -1.0), d, false, x0, ew),
        ];
        let mut g = Geo { l, clear, sides, windows: vec![], doors: vec![], columns: vec![], wet: [0, 1, 2, 3] };
        // a small wet room's door opens outwards: only the frame strip stays free inside
        let area = (x1 - x0) * (y1 - y0);
        let outwards = matches!(me.kind, RoomKind::Wc | RoomKind::Bath) && area < rules.outward_below;
        g.openings(if outwards { Some(0.3) } else { None }, rules.door_gap);
        g.doors.extend(me.keep.iter().copied());
        for c in &l.columns {
            let (hx, hy) = (c.section.0 / 2.0 + STEP, c.section.1 / 2.0 + STEP);
            g.columns.push([c.at.0 - hx, c.at.1 - hy, c.at.0 + hx, c.at.1 + hy]);
        }
        g.wet = g.wet_order(me, rooms);
        g
    }

    /// Door swings and windows by side.
    fn openings(&mut self, swing: Option<f64>, gap: f64) {
        for o in &self.l.openings {
            let wall = &self.l.walls[o.wall];
            let (dx, dy) = (wall.b.0 - wall.a.0, wall.b.1 - wall.a.1);
            let len = dx.hypot(dy);
            if len < EPS {
                continue;
            }
            // on an arc wall the ends sit on the arc, over the chord a -> b
            let (p0, p1) = (wall.at(o.along), wall.at(o.along + o.width));
            let horiz = dy.abs() < EPS;
            let line = if horiz { wall.a.1 } else { wall.a.0 };
            for k in 0..4 {
                let sd = self.sides[k];
                if sd.horiz != horiz || (sd.line - line).abs() > EPS {
                    continue;
                }
                let (a, b) = (sd.s_of(p0), sd.s_of(p1));
                let (s0, s1) = (a.min(b), a.max(b));
                if s1 <= EPS || s0 >= sd.len - EPS {
                    continue;
                }
                if o.door {
                    let deep = swing.unwrap_or(o.width.min(1.2) + gap);
                    self.doors.push(sd.band(s0 - gap, s1 + gap, 0.0, deep));
                } else {
                    self.windows.push((k, s0.max(0.0), s1.min(sd.len)));
                }
            }
        }
    }

    /// The wet side: shared with another wet room or a shaft, no window, no door, longest.
    fn wet_order(&self, me: &RoomIn<'_>, rooms: &[RoomIn<'_>]) -> [usize; 4] {
        let along = |horiz: bool, r: &R| if horiz { (r[0], r[2]) } else { (r[1], r[3]) };
        let score = |k: usize| {
            let sd = &self.sides[k];
            let mine = along(sd.horiz, &me.rect);
            let overlap = |o: (f64, f64)| o.1.min(mine.1) - o.0.max(mine.0);
            let on_line = |r: &R, tol: f64| {
                let (a, b) = if sd.horiz { (r[1], r[3]) } else { (r[0], r[2]) };
                (a - sd.line).abs() < tol || (b - sd.line).abs() < tol
            };
            let mut s = sd.len * 0.1;
            if rooms.iter().any(|r| {
                !std::ptr::eq(r, me) && wet_kind(r.kind) && on_line(&r.rect, 1e-6) && overlap(along(sd.horiz, &r.rect)) > 0.3
            }) {
                s += 4.0;
            }
            for sh in &self.l.shafts {
                let xs = sh.ring.iter().map(|p| p.0);
                let ys = sh.ring.iter().map(|p| p.1);
                let bb = [
                    xs.clone().fold(f64::MAX, f64::min),
                    ys.clone().fold(f64::MAX, f64::min),
                    xs.fold(f64::MIN, f64::max),
                    ys.fold(f64::MIN, f64::max),
                ];
                if on_line(&bb, 0.5) && overlap(along(sd.horiz, &bb)) > 0.1 {
                    s += 4.0;
                }
            }
            if self.windows.iter().any(|w| w.0 == k) {
                s -= 2.0;
            }
            let face = sd.band(0.0, sd.len, 0.0, 0.05);
            if self.doors.iter().any(|d| hit(d, &face)) {
                s -= 1.0;
            }
            s
        };
        let wet = (0..4).fold(0, |b, k| if score(k) > score(b) + EPS { k } else { b });
        [wet, (wet + 1) % 4, (wet + 3) % 4, (wet + 2) % 4]
    }

    pub fn area(&self) -> f64 {
        (self.clear[2] - self.clear[0]) * (self.clear[3] - self.clear[1])
    }

    fn depth(&self, k: usize) -> f64 {
        if self.sides[k].horiz {
            self.clear[3] - self.clear[1]
        } else {
            self.clear[2] - self.clear[0]
        }
    }

    fn windowed(&self, k: usize, s0: f64, s1: f64) -> bool {
        self.windows.iter().any(|&(j, a, b)| j == k && a < s1 - EPS && s0 < b - EPS)
    }

    /// The strip before each window that tall pieces keep off (as the audit reads it).
    fn before_windows(&self) -> Vec<R> {
        self.windows.iter().map(|&(k, a, b)| self.sides[k].band(a, b, 0.0, 1.0)).collect()
    }

    /// Sides longest first, those with windows last when `dark` asks for walls without.
    fn order(&self, dark: bool) -> Vec<usize> {
        let mut ks = vec![0, 1, 2, 3];
        ks.sort_by(|&a, &b| {
            let wa = dark && self.windows.iter().any(|w| w.0 == a);
            let wb = dark && self.windows.iter().any(|w| w.0 == b);
            wa.cmp(&wb).then(self.sides[b].len.total_cmp(&self.sides[a].len))
        });
        ks
    }
}

/// The wall on that axis line covering `a..b` along it.
fn wall_at(l: &GLayout, horiz: bool, line: f64, a: f64, b: f64) -> Option<usize> {
    let (lo, hi) = (a.min(b), a.max(b));
    l.walls.iter().position(|w| {
        let on = if horiz {
            (w.a.1 - line).abs() < EPS && (w.b.1 - line).abs() < EPS
        } else {
            (w.a.0 - line).abs() < EPS && (w.b.0 - line).abs() < EPS
        };
        let (p, q) = if horiz { (w.a.0, w.b.0) } else { (w.a.1, w.b.1) };
        on && p.min(q) <= lo + EPS && p.max(q) >= hi - EPS
    })
}

/// The wall whose room face holds a wall-hosted piece at `at` looking along `front`.
pub(crate) fn host_wall(l: &GLayout, at: (f64, f64), front: (f64, f64), body: &R) -> Option<usize> {
    let horiz = front.1.abs() > 0.5;
    l.walls.iter().position(|w| {
        let half = w.role.thickness() / 2.0;
        if horiz {
            (w.a.1 - w.b.1).abs() < EPS
                && (at.1 - front.1 * half - w.a.1).abs() < 1e-6
                && w.a.0.min(w.b.0) <= body[0] + EPS
                && w.a.0.max(w.b.0) >= body[2] - EPS
        } else {
            (w.a.0 - w.b.0).abs() < EPS
                && (at.0 - front.0 * half - w.a.0).abs() < 1e-6
                && w.a.1.min(w.b.1) <= body[1] + EPS
                && w.a.1.max(w.b.1) >= body[3] - EPS
        }
    })
}

/// Distance from a rectangle to a point.
pub(crate) fn rect_dist(r: &R, p: (f64, f64)) -> f64 {
    let dx = (r[0] - p.0).max(p.0 - r[2]).max(0.0);
    let dy = (r[1] - p.1).max(p.1 - r[3]).max(0.0);
    dx.hypot(dy)
}

/// What a room holds while it is being fitted out.
#[derive(Clone)]
struct State {
    /// Floor bodies (columns first) and raised bodies.
    taken: Vec<R>,
    raised: Vec<R>,
    /// Free space: no body may take it.
    keep: Vec<R>,
    out: Vec<GFixture>,
    /// Item of each fixture, and the side it was set against.
    items: Vec<(usize, usize)>,
    falls: Vec<f64>,
    turning: Vec<((f64, f64), f64)>,
    triangle: Option<[f64; 3]>,
    dwell: HashSet<(u32, usize)>,
}

/// One set laid out at a place, not yet committed.
struct Plan {
    bodies: Vec<(R, bool)>,
    keeps: Vec<R>,
    fx: Vec<(GFixture, usize)>,
    side: usize,
    fall: Option<f64>,
}

struct Cx<'g, 'a> {
    g: &'g Geo<'a>,
    rules: &'g Rules,
    st: State,
    /// The dwelling the room belongs to, if any.
    dwelling: Option<u32>,
    typology: Typology,
    /// Why spots were refused since the last reset: count per (reason of [`REFUSALS`], piece).
    refused: std::cell::RefCell<std::collections::BTreeMap<(usize, Option<usize>), u32>>,
}

/// What can refuse a spot to a set, in the order [`Cx::plan`] checks it.
const REFUSALS: [&str; 8] = [
    "past the side's end",
    "off the clear floor or on another body",
    "its front space taken",
    "a tall set before a window",
    "a tall piece before a window strip",
    "no wall face holds it",
    "a piece's front band taken",
    "a piece's side bands taken",
];

/// The dwelling of a room: its apartment, or the whole house.
pub(crate) fn dwelling_of(owner: PRole, typology: Typology) -> Option<u32> {
    match owner {
        PRole::Apartment(a) => Some(a),
        PRole::Public if typology == Typology::House => Some(u32::MAX),
        PRole::Public => None,
    }
}

impl Cx<'_, '_> {
    fn sides(&self, sel: &Sides) -> Vec<usize> {
        let placed = |i: usize| {
            let mut ks: Vec<usize> = vec![];
            for &(it, k) in &self.st.items {
                if it == i && !ks.contains(&k) {
                    ks.push(k);
                }
            }
            ks
        };
        match sel {
            Sides::All => self.g.order(false),
            Sides::Dark => self.g.order(true),
            Sides::Wet => self.g.wet.to_vec(),
            Sides::Longest => vec![self.g.order(false)[0]],
            Sides::Others => self.g.order(false)[1..].to_vec(),
            Sides::Of(i) => placed(*i),
            Sides::Opposite(i) => placed(*i).into_iter().map(|k| (k + 2) % 4).collect(),
        }
    }

    fn count(&self, item: usize) -> usize {
        self.st.items.iter().filter(|x| x.0 == item).count()
    }

    fn free_floor(&self, r: &R, own: &[(R, bool)]) -> bool {
        inside(r, &self.g.clear)
            && !self.st.taken.iter().chain(&self.g.columns).any(|t| hit(t, r))
            && !own.iter().any(|(b, raised)| !raised && hit(b, r))
    }

    /// Counts one refused spot under reason `i` of [`REFUSALS`], for piece `item` of the set if
    /// one piece refused it.
    fn no(&self, i: usize, item: Option<usize>) -> Option<Plan> {
        *self.refused.borrow_mut().entry((i, item)).or_insert(0) += 1;
        None
    }

    /// The refusals counted since the last call, most frequent first, as "n [item:] reason".
    fn take_refusals(&self) -> String {
        let mut v: Vec<((usize, Option<usize>), u32)> = self.refused.take().into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        if v.is_empty() {
            return "no side long enough to try".to_string();
        }
        v.iter()
            .map(|((i, item), n)| match item {
                Some(k) => format!("{n} {}: {}", self.rules.items[*k].id, REFUSALS[*i]),
                None => format!("{n} {}", REFUSALS[*i]),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Set `set` against side `k` from `u0` along it and `v0` into the room, if it fits.
    fn plan(&self, k: usize, u0: f64, v0: f64, set: &Set, mirror: bool, front: f64) -> Option<Plan> {
        let g = self.g;
        let sd = g.sides[k];
        if u0 < -EPS || u0 + set.w > sd.len + EPS {
            return self.no(0, None);
        }
        let zone = sd.band(u0, u0 + set.w, v0, v0 + set.d);
        let blocked = |r: &R| {
            self.st.taken.iter().chain(&self.st.raised).chain(&self.st.keep).chain(&g.doors).chain(&g.columns).any(|t| hit(t, r))
        };
        if !inside(&zone, &g.clear) || blocked(&zone) {
            return self.no(1, None);
        }
        let mut plan = Plan { bodies: vec![], keeps: vec![zone], fx: vec![], side: k, fall: None };
        if front > 0.0 {
            let fr = sd.band(u0, u0 + set.w, v0 + set.d, v0 + set.d + front);
            if !self.free_floor(&fr, &[]) {
                return self.no(2, None);
            }
            plan.keeps.push(fr);
        }
        if v0 < 1.0 && set.tall(self.rules) && g.windowed(k, u0, u0 + set.w) {
            return self.no(3, None);
        }
        for p in &set.parts {
            let it = &self.rules.items[p.item];
            let face = if mirror { p.face.mirrored() } else { p.face };
            let (eu, ev) = face.extent(it.w, it.d);
            let pu = if mirror { set.w - p.at.0 - eu } else { p.at.0 };
            let body = sd.band(u0 + pu, u0 + pu + eu, v0 + p.at.1, v0 + p.at.1 + ev);
            // a tall body keeps off the strip before every window of the room, not only those
            // of its own side: at a corner it reaches into the next side's strip
            if it.tall && g.before_windows().iter().any(|w| hit(w, &body)) {
                return self.no(4, Some(p.item));
            }
            let fv = sd.front(face);
            let (at, rotation) = fixture_at(it, body, fv);
            let wall = if it.org == Org::Wall {
                match host_wall(g.l, at, fv, &body) {
                    Some(w) => Some(w),
                    None => return self.no(5, Some(p.item)),
                }
            } else {
                None
            };
            let f = GFixture { scale: 1.0, min_clear: 0.0, family: it.fam, ty: it.ty, at, rotation, wall, z: 0.0 };
            if it.on.is_empty() {
                plan.bodies.push((body, it.raised));
            }
            plan.fx.push((f, p.item));
        }
        // each piece's own free space, against what stands and the set's other bodies (not its
        // own, nor the counter a sink sits in)
        for (f, item) in &plan.fx {
            let it = &self.rules.items[*item];
            let pl = Placed::of(it, f);
            let others: Vec<(R, bool)> = plan.bodies.iter().copied().filter(|(b, _)| !hit(b, &pl.body)).collect();
            if let Some(fb) = pl.front_band(it) {
                if !self.free_floor(&fb, &others) {
                    return self.no(6, Some(*item));
                }
                plan.keeps.push(fb);
            }
            let free: Vec<R> = pl.side_bands(it).into_iter().filter(|b| self.free_floor(b, &others)).collect();
            if free.len() < it.side.2 as usize {
                return self.no(7, Some(*item));
            }
            plan.keeps.extend(free);
        }
        if set.fall {
            let drain = plan.fx.iter().find(|(_, i)| self.rules.items[*i].drain.is_some() && self.rules.items[*i].org == Org::Centre);
            if let Some((f, _)) = drain {
                let far = [(zone[0], zone[1]), (zone[2], zone[1]), (zone[2], zone[3]), (zone[0], zone[3])]
                    .iter()
                    .map(|c| (c.0 - f.at.0).hypot(c.1 - f.at.1))
                    .fold(0.0, f64::max);
                plan.fall = Some(far * self.rules.drainage.floor_fall);
            }
        }
        Some(plan)
    }

    fn commit(&mut self, p: Plan) {
        for (b, raised) in p.bodies {
            if raised {
                self.st.raised.push(b);
            } else {
                self.st.taken.push(b);
            }
        }
        self.st.keep.extend(p.keeps);
        for (f, item) in p.fx {
            self.st.out.push(f);
            self.st.items.push((item, p.side));
        }
        if let Some(f) = p.fall {
            self.st.falls.push(f);
        }
    }

    /// Branches past their rules once plan `p` stands too, from the stack that serves them best.
    fn over_with(&self, p: &Plan) -> usize {
        let fx = self.st.out.iter().zip(self.st.items.iter().map(|x| x.0)).chain(p.fx.iter().map(|(f, i)| (f, *i)));
        stack_for(self.rules, self.g, &drained_of(self.rules, fx)).map_or(0, |s| s.1)
    }

    /// One of the sets against one of the sides, first fit: from the start, or nearest the middle.
    /// A drained set takes the first spot whose branch, and every branch before it, keeps
    /// within the drain rules; with none, the spot that leaves the fewest past them.
    fn put(&mut self, what: &[usize], sides: &[usize], centred: bool) -> bool {
        let rules = self.rules;
        let drained = |si: usize| rules.sets[si].parts.iter().any(|p| rules.items[p.item].drain.is_some());
        let mut fallback: Option<(Plan, usize)> = None;
        for &si in what {
            let set = &self.rules.sets[si];
            for &k in sides {
                let len = self.g.sides[k].len;
                if len + EPS < set.w {
                    continue;
                }
                let spots: Vec<(f64, bool)> = if set.corner {
                    vec![(0.0, false), (len - set.w, true)]
                } else {
                    let n = ((len - set.w) / STEP).floor() as usize;
                    let mut ss: Vec<f64> = (0..=n).map(|i| i as f64 * STEP).collect();
                    // a drained set shares its wet side with the next drained piece: centred
                    // while each end it leaves holds a set as wide, else the spare length stays
                    // in one piece at an end
                    let mid = (len - set.w) / 2.0;
                    if centred && (!drained(si) || mid + EPS >= set.w) {
                        ss.push(mid);
                        ss.sort_by(|a, b| (a - mid).abs().total_cmp(&(b - mid).abs()));
                    }
                    ss.into_iter().map(|s| (s, false)).collect()
                };
                for (s, mirror) in spots {
                    if let Some(p) = self.plan(k, s, 0.0, set, mirror, set.front) {
                        let over = if drained(si) { self.over_with(&p) } else { 0 };
                        if over == 0 {
                            self.commit(p);
                            return true;
                        }
                        if fallback.as_ref().is_none_or(|f| over < f.1) {
                            fallback = Some((p, over));
                        }
                    }
                }
            }
        }
        match fallback {
            Some((p, _)) => {
                self.commit(p);
                true
            }
            None => false,
        }
    }

    /// Runs the steps; returns what they asked for and did not get.
    fn steps(&mut self, steps: &[Step]) -> Vec<String> {
        let mut unmet = vec![];
        for s in steps {
            unmet.extend(self.step(s));
        }
        unmet
    }

    fn step(&mut self, s: &Step) -> Vec<String> {
        let area = self.g.area();
        let name = |r: &Rules, set: usize| r.items[r.sets[set].key()].id.clone();
        match s {
            Step::Place { what, sides, centred, count, need, once, min_area, typologies, when } => {
                let here = *when && (typologies.is_empty() || typologies.contains(&self.typology));
                if area + EPS < *min_area || what.is_empty() || !here {
                    return vec![];
                }
                let key = self.rules.sets[what[0]].key();
                let dwelling = self.dwelling.filter(|_| *once);
                if dwelling.is_some_and(|a| self.st.dwell.contains(&(a, key))) {
                    return vec![];
                }
                let target = match count {
                    Count::N(n) => *n as usize,
                    Count::Per { item, ratio, min } => ((self.count(*item) as f64 * ratio).ceil() as usize).max(*min as usize),
                };
                let ks = self.sides(sides);
                let mut placed = 0;
                self.refused.take();
                while placed < target && self.put(what, &ks, *centred) {
                    self.refused.take();
                    placed += 1;
                }
                if let (Some(a), true) = (dwelling, placed > 0) {
                    self.st.dwell.insert((a, key));
                }
                if placed < *need as usize {
                    let why = self.take_refusals();
                    return vec![format!("{}: {placed} of {need} (spots refused: {why})", name(self.rules, what[0]))];
                }
                vec![]
            }
            Step::Choose(options) => {
                let save = self.st.clone();
                let mut first: Option<(Vec<String>, State)> = None;
                for o in options {
                    let unmet = self.steps(o);
                    if unmet.is_empty() {
                        return vec![];
                    }
                    if first.is_none() {
                        first = Some((unmet, self.st.clone()));
                    }
                    self.st = save.clone();
                }
                match first {
                    Some((unmet, st)) => {
                        self.st = st;
                        unmet
                    }
                    None => vec![],
                }
            }
            Step::Grid { set, module, start, max, rows, sides, need, min_area, occupancy } => {
                if area + EPS < *min_area {
                    return vec![];
                }
                let max = match occupancy {
                    Some((a, n)) => (*max).min(((area + EPS) / (a * f64::from((*n).max(1)))).floor() as u32).max(*need),
                    None => *max,
                };
                let max = &max;
                let st = &self.rules.sets[*set];
                let mut placed = 0;
                self.refused.take();
                // the named sides first; a room none of them holds a set on tries its others
                let mut ks = self.sides(sides);
                ks.extend(self.g.order(false).into_iter().filter(|k| !ks.contains(k)).collect::<Vec<_>>());
                for k in ks {
                    let sd = self.g.sides[k];
                    let depth = self.g.depth(k);
                    // the module's spare room lies between sets: the last column and row need
                    // only the set itself
                    let cols = if sd.len + EPS < st.w { 0 } else { ((sd.len - st.w + EPS) / module.0).floor() as usize + 1 };
                    let nrows = if depth + EPS < start + st.d {
                        0
                    } else {
                        (((depth - start - st.d + EPS) / module.1).floor() as usize + 1).min(*rows as usize)
                    };
                    let span = cols.saturating_sub(1) as f64 * module.0 + st.w;
                    let s0 = (sd.len - span) / 2.0 - (module.0 - st.w) / 2.0;
                    for r in 0..nrows {
                        for c in 0..cols {
                            if placed >= *max as usize {
                                break;
                            }
                            let u0 = s0 + c as f64 * module.0 + (module.0 - st.w) / 2.0;
                            let v0 = start + r as f64 * module.1;
                            if let Some(p) = self.plan(k, u0, v0, st, false, st.front) {
                                self.commit(p);
                                placed += 1;
                            }
                        }
                    }
                    if placed > 0 {
                        break;
                    }
                }
                if placed < *need as usize {
                    let why = self.take_refusals();
                    return vec![format!("{}: {placed} of {need} on the grid (spots refused: {why})", name(self.rules, *set))];
                }
                vec![]
            }
            Step::Attach { item, to, beside, gap, max, need } => {
                let (hosts, got) = self.attach(*item, to, *beside, *gap, *max as usize);
                if *need > 0 && got < hosts.min(*max as usize) {
                    return vec![format!("{}: {got} of {}", self.rules.items[*item].id, hosts.min(*max as usize))];
                }
                vec![]
            }
            Step::Run { run, sides, need } => {
                let ok = self.run(&self.rules.runs[*run], sides);
                if !ok && *need > 0 {
                    return vec![format!("run {} does not fit", self.rules.runs[*run].id)];
                }
                vec![]
            }
            Step::Windows(what) => {
                for (k, a, b) in self.g.windows.clone() {
                    if !self.g.sides[k].ext {
                        continue;
                    }
                    let mid = (a + b) / 2.0;
                    for &si in what {
                        let set = &self.rules.sets[si];
                        if b - a + EPS < set.w {
                            continue;
                        }
                        if let Some(p) = self.plan(k, mid - set.w / 2.0, 0.0, set, false, set.front) {
                            self.commit(p);
                            break;
                        }
                    }
                }
                vec![]
            }
            Step::Turning { d, need } => {
                let mut got = 0;
                while got < *need as usize && self.turning(*d) {
                    got += 1;
                }
                if got < *need as usize {
                    return vec![format!("turning circle {d} m: {got} of {need}")];
                }
                vec![]
            }
            Step::Bays { set, stall, aisle, need } => {
                let n = self.bays(&self.rules.sets[*set], *stall, *aisle);
                if n < *need as usize {
                    return vec![format!("{}: {n} of {need} bays", name(self.rules, *set))];
                }
                vec![]
            }
        }
    }

    /// `item` on or beside each placed piece of the `to` items, against the same wall and
    /// looking the same way, at most `max`: (hosts, how many got one). A piece on its host takes
    /// no space of its own; one beside it keeps off every body, door swing and column.
    fn attach(&mut self, item: usize, to: &[usize], beside: bool, gap: f64, max: usize) -> (usize, usize) {
        let rules = self.rules;
        let it = &rules.items[item];
        let hosts: Vec<(R, (f64, f64), usize)> = self
            .st
            .out
            .iter()
            .zip(&self.st.items)
            .filter(|(_, (i, _))| to.contains(i))
            .map(|(f, &(i, k))| {
                let p = Placed::of(&rules.items[i], f);
                (p.body, p.front, k)
            })
            .collect();
        let windows = self.g.before_windows();
        let mut got = 0;
        for &(hb, fr, k) in &hosts {
            if got >= max {
                break;
            }
            // the host's back edge is on the wall; the piece reaches `it.d` from it
            let along_x = fr.1.abs() > 0.5;
            let (h0, h1) = if along_x { (hb[0], hb[2]) } else { (hb[1], hb[3]) };
            let strip = |s0: f64, s1: f64| -> R {
                match (along_x, fr.0 + fr.1 > 0.0) {
                    (true, true) => [s0, hb[1], s1, hb[1] + it.d],
                    (true, false) => [s0, hb[3] - it.d, s1, hb[3]],
                    (false, true) => [hb[0], s0, hb[0] + it.d, s1],
                    (false, false) => [hb[2] - it.d, s0, hb[2], s1],
                }
            };
            let mid = (h0 + h1) / 2.0;
            let spots: Vec<R> = if beside {
                vec![strip(h1 + gap, h1 + gap + it.w), strip(h0 - gap - it.w, h0 - gap)]
            } else if it.w <= h1 - h0 + EPS {
                vec![strip(mid - it.w / 2.0, mid + it.w / 2.0)]
            } else {
                vec![]
            };
            for body in spots {
                let g = self.g;
                let clash = |r: &R| {
                    self.st.taken.iter().chain(&self.st.raised).chain(&g.doors).chain(&g.columns).any(|t| hit(t, r))
                };
                if !inside(&body, &g.clear) || (beside && clash(&body)) || (it.tall && windows.iter().any(|w| hit(w, &body))) {
                    continue;
                }
                let (at, rotation) = fixture_at(it, body, fr);
                let wall = if it.org == Org::Wall {
                    match host_wall(g.l, at, fr, &body) {
                        Some(w) => Some(w),
                        None => continue,
                    }
                } else {
                    None
                };
                self.st.out.push(GFixture { scale: 1.0, min_clear: 0.0, family: it.fam, ty: it.ty, at, rotation, wall, z: 0.0 });
                self.st.items.push((item, k));
                if beside {
                    if it.raised {
                        self.st.raised.push(body);
                    } else {
                        self.st.taken.push(body);
                    }
                }
                got += 1;
                break;
            }
        }
        (hosts.len(), got)
    }

    /// A free wheelchair circle of diameter `d` nearest the room centre: no floor body in it.
    fn turning(&mut self, d: f64) -> bool {
        let c = self.g.clear;
        let mid = ((c[0] + c[2]) / 2.0, (c[1] + c[3]) / 2.0);
        let r = d / 2.0;
        let mut best: Option<(f64, (f64, f64))> = None;
        let mut y = c[1] + r;
        while y <= c[3] - r + EPS {
            let mut x = c[0] + r;
            while x <= c[2] - r + EPS {
                let p = (x, y);
                let clear = !self.st.taken.iter().chain(&self.g.columns).any(|t| rect_dist(t, p) < r - EPS)
                    && !self.st.turning.iter().any(|(q, e)| (q.0 - x).hypot(q.1 - y) < r + e / 2.0 - EPS);
                let dist = (x - mid.0).hypot(y - mid.1);
                if clear && best.is_none_or(|b| dist < b.0 - EPS) {
                    best = Some((dist, p));
                }
                x += STEP;
            }
            y += STEP;
        }
        let Some((_, p)) = best else {
            return false;
        };
        self.st.keep.push([p.0 - r, p.1 - r, p.0 + r, p.1 + r]);
        self.st.turning.push((p, d));
        true
    }

    /// Bays off the longest wall: nose to the wall, aisle, a row nosing the next one, and so on;
    /// every bay on its own so a column costs one bay only.
    fn bays(&mut self, set: &Set, stall: (f64, f64), aisle: f64) -> usize {
        let k = self.g.order(false)[0];
        let sd = self.g.sides[k];
        let depth = self.g.depth(k);
        let cols = ((sd.len + EPS) / stall.0).floor() as usize;
        let s0 = (sd.len - cols as f64 * stall.0) / 2.0;
        let (mut v, mut down, mut n) = (0.0, true, 0);
        let mut one = set.clone();
        one.w = stall.0;
        one.d = stall.1;
        let it = &self.rules.items[set.key()];
        one.parts[0].at = ((stall.0 - it.w) / 2.0, (stall.1 - it.d) / 2.0);
        while v + stall.1 <= depth + EPS {
            one.parts[0].face = if down { Face::In } else { Face::Out };
            for c in 0..cols {
                let u = s0 + c as f64 * stall.0;
                if let Some(p) = self.plan(k, u, v, &one, false, 0.0) {
                    self.commit(p);
                    n += 1;
                }
            }
            v += stall.1;
            if down {
                if v + aisle + stall.1 > depth + EPS {
                    break;
                }
                v += aisle;
            }
            down = !down;
        }
        n
    }

    /// A kitchen run: on one wall or turning one corner, first the layouts with every optional
    /// module, the work triangle met where the rule asks for it.
    fn run(&mut self, run: &Run, sel: &Sides) -> bool {
        let sides = self.sides(sel);
        let need_tri = run.triangle.as_ref().is_some_and(|t| self.g.area() + EPS >= t.from_area);
        let mut seqs: Vec<Vec<Slot>> = vec![];
        for s in &run.seqs {
            seqs.push(s.clone());
            let mut r = s.clone();
            r.reverse();
            if r != *s {
                seqs.push(r);
            }
        }
        let max_drop = seqs
            .iter()
            .map(|s| s.iter().filter(|x| matches!(x, Slot::Module { optional: true, .. })).count())
            .max()
            .unwrap_or(0);
        let save = self.st.clone();
        let mut fallback: Option<State> = None;
        for drop in 0..=max_drop {
            for seq in &seqs {
                let opts: Vec<usize> = (0..seq.len()).filter(|&i| matches!(seq[i], Slot::Module { optional: true, .. })).collect();
                for gone in subsets(&opts, drop) {
                    let slots: Vec<Slot> =
                        seq.iter().enumerate().filter(|(i, _)| !gone.contains(i)).map(|(_, s)| s.clone()).collect();
                    for &k in &sides {
                        for turn in [false, true] {
                            if turn && !run.turn {
                                continue;
                            }
                            for s0 in self.starts(k, run.depth) {
                                let lay = Lay::new(self.g, k, s0, turn, run.depth);
                                if lay.la <= 0.0 {
                                    continue;
                                }
                                match self.dfs(run, &lay, &slots, 0, 0.0, false, need_tri) {
                                    Some(true) => return true,
                                    Some(false) => {
                                        if fallback.is_none() {
                                            fallback = Some(self.st.clone());
                                        }
                                    }
                                    None => {}
                                }
                                self.st = save.clone();
                            }
                        }
                    }
                }
            }
        }
        match fallback {
            Some(st) => {
                self.st = st;
                true
            }
            None => {
                self.st = save;
                false
            }
        }
    }

    /// Where a run may start on side `k`: the corner, or just past a door swing or a body.
    fn starts(&self, k: usize, depth: f64) -> Vec<f64> {
        let sd = self.g.sides[k];
        let strip = sd.band(0.0, sd.len, 0.0, depth);
        let mut out = vec![0.0];
        for r in self.g.doors.iter().chain(&self.st.taken).chain(&self.g.columns) {
            if hit(r, &strip) {
                let (a, b) = (sd.s_of((r[0], r[1])), sd.s_of((r[2], r[3])));
                let e = (a.max(b) / STEP).ceil() * STEP;
                if e > 0.0 && e < sd.len - 0.3 && !out.iter().any(|x: &f64| (x - e).abs() < EPS) {
                    out.push(e);
                }
            }
        }
        out
    }

    /// Lays `slots[i..]` from path position `p`. `Some(true)`: done with the triangle met (or not
    /// asked); `Some(false)`: laid out but the triangle missed, the layout left in `st`.
    #[allow(clippy::too_many_arguments)]
    fn dfs(&mut self, run: &Run, lay: &Lay, slots: &[Slot], i: usize, p: f64, on_b: bool, need_tri: bool) -> Option<bool> {
        if i == slots.len() {
            return self.finish(run, lay, p, on_b, need_tri);
        }
        let save = self.st.clone();
        match &slots[i] {
            Slot::Gap(gs) => {
                let mut fallback: Option<State> = None;
                for &g in gs {
                    let mut b = on_b;
                    if self.fill(run, lay, p, p + g, &mut b) {
                        match self.dfs(run, lay, slots, i + 1, p + g, b, need_tri) {
                            Some(true) => return Some(true),
                            Some(false) if fallback.is_none() => fallback = Some(self.st.clone()),
                            _ => {}
                        }
                    }
                    self.st = save.clone();
                }
                fallback.map(|st| {
                    self.st = st;
                    false
                })
            }
            Slot::Module { set, .. } => {
                let set = &self.rules.sets[*set];
                let mut p = p;
                let mut b = on_b;
                if lay.turn && p < lay.la - EPS && p + set.w > lay.la + EPS {
                    // jump the corner, filling up to it
                    if !self.fill(run, lay, p, lay.la, &mut b) {
                        return None;
                    }
                    p = lay.la;
                }
                let (k, s) = lay.map(p, set.w)?;
                let front = run.front.max(set.front);
                let plan = self.plan(k, s, 0.0, set, false, front)?;
                self.commit(plan);
                let r = self.dfs(run, lay, slots, i + 1, p + set.w, b || k != lay.k, need_tri);
                if r.is_none() {
                    self.st = save;
                }
                r
            }
        }
    }

    /// Fills the path from `p0` to `p1` with filler counters; a strip may stay at each end.
    fn fill(&mut self, run: &Run, lay: &Lay, p0: f64, p1: f64, on_b: &mut bool) -> bool {
        if p1 - p0 < EPS {
            return true;
        }
        let pieces = if lay.turn && p0 < lay.la - EPS && p1 > lay.la + EPS {
            vec![(p0, lay.la), (lay.la, p1)]
        } else {
            vec![(p0, p1)]
        };
        for (a, b) in pieces {
            let mut p = a;
            while b - p > STRIP + EPS {
                let room = b - p;
                let rules = self.rules;
                let filler = |f: usize| &rules.sets[rules.item_sets[f][0]];
                let Some(&f) = run.fill.iter().find(|&&f| filler(f).w <= room + EPS) else {
                    return false;
                };
                let set = filler(f);
                let Some((k, s)) = lay.map(p, set.w) else {
                    return false;
                };
                let Some(plan) = self.plan(k, s, 0.0, set, false, run.front) else {
                    return false;
                };
                self.commit(plan);
                *on_b |= k != lay.k;
                p += set.w;
            }
        }
        true
    }

    /// Closes a laid-out run: the corner piece, the worktop beyond, the triangle.
    fn finish(&mut self, run: &Run, lay: &Lay, p: f64, on_b: bool, need_tri: bool) -> Option<bool> {
        if lay.turn && !on_b {
            return None;
        }
        if lay.turn {
            if let Some(c) = run.corner {
                let set = &self.rules.sets[self.rules.item_sets[c][0]];
                let sd = self.g.sides[lay.k];
                let plan = self.plan(lay.k, sd.len - run.depth, 0.0, set, false, 0.0)?;
                self.commit(plan);
            }
        }
        // more worktop beyond the last module while it fits
        let mut q = p;
        while q - p < run.tail - EPS {
            let next = run.fill.iter().find_map(|&f| {
                let set = &self.rules.sets[self.rules.item_sets[f][0]];
                if q + set.w - p > run.tail + EPS {
                    return None;
                }
                let (k, s) = lay.map(q, set.w)?;
                Some((self.plan(k, s, 0.0, set, false, run.front)?, set.w))
            });
            let Some((plan, w)) = next else {
                break;
            };
            self.commit(plan);
            q += w;
        }
        let Some(t) = &run.triangle else {
            return Some(true);
        };
        let centre = |item: usize| {
            self.st.out.iter().zip(&self.st.items).rev().find(|(_, x)| x.0 == item).map(|(f, x)| {
                let b = Placed::of(&self.rules.items[x.0], f).body;
                ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0)
            })
        };
        let pts: Vec<(f64, f64)> = t.items.iter().filter_map(|&i| centre(i)).collect();
        if pts.len() < 3 {
            self.st.triangle = None;
            return Some(!need_tri);
        }
        let d = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1);
        let legs = [d(pts[0], pts[1]), d(pts[1], pts[2]), d(pts[2], pts[0])];
        self.st.triangle = Some(legs);
        Some(!need_tri || triangle_ok(&legs, t.leg, t.sum))
    }
}

pub(crate) fn triangle_ok(legs: &[f64; 3], leg: (f64, f64), sum: (f64, f64)) -> bool {
    let s: f64 = legs.iter().sum();
    legs.iter().all(|l| *l + EPS >= leg.0 && *l <= leg.1 + EPS) && s + EPS >= sum.0 && s <= sum.1 + EPS
}

/// Index sets of `n` elements out of `from`, in order.
fn subsets(from: &[usize], n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![vec![]];
    }
    let mut out = vec![];
    for (i, &x) in from.iter().enumerate() {
        for mut rest in subsets(&from[i + 1..], n - 1) {
            rest.insert(0, x);
            out.push(rest);
        }
    }
    out
}

/// The path of a run: side `k` from `s0`, then round the corner square onto the next side.
struct Lay {
    k: usize,
    k1: usize,
    s0: f64,
    depth: f64,
    turn: bool,
    la: f64,
    lb: f64,
}

impl Lay {
    fn new(g: &Geo<'_>, k: usize, s0: f64, turn: bool, depth: f64) -> Lay {
        let k1 = (k + 1) % 4;
        let len = g.sides[k].len;
        let la = if turn { len - depth } else { len } - s0;
        let lb = if turn { g.sides[k1].len - depth } else { 0.0 };
        Lay { k, k1, s0, depth, turn, la, lb }
    }

    fn map(&self, p: f64, w: f64) -> Option<(usize, f64)> {
        if p + w <= self.la + EPS {
            Some((self.k, self.s0 + p))
        } else if self.turn && p >= self.la - EPS && p + w <= self.la + self.lb + EPS {
            Some((self.k1, self.depth + (p - self.la)))
        } else {
            None
        }
    }
}

/// Fits out every room of a storey; `h` is the storey height. The fixtures come in room order.
/// `dwell` holds the (dwelling, set) pairs placed once per dwelling: a dwelling spans storeys,
/// so the caller carries it from one storey to the next.
pub(crate) fn furnish(
    l: &GLayout,
    rooms: &[RoomIn<'_>],
    h: f64,
    typology: Typology,
    rules: &Rules,
    dwell: &mut HashSet<(u32, usize)>,
) -> (Vec<GFixture>, Vec<RoomFit>) {
    let geos: Vec<Geo<'_>> = rooms.iter().map(|r| Geo::new(l, r, rooms, rules)).collect();
    let facts = |i: usize| {
        let g = &geos[i];
        let (w, d) = (g.clear[2] - g.clear[0], g.clear[3] - g.clear[1]);
        super::fit_rules::RoomFacts {
            kind: rooms[i].kind,
            dwelling: dwelling_of(rooms[i].owner, typology).is_some(),
            typology,
            area: g.area(),
            width: w.min(d),
            height: h,
            name: rooms[i].name,
            tags: rooms[i].tags,
        }
    };
    let prio = |i: usize| rules.rooms.iter().find(|x| x.matches(&facts(i))).map_or(i64::MAX, |x| x.priority);
    let mut order: Vec<usize> = (0..rooms.len()).collect();
    order.sort_by_key(|&i| (prio(i), i));
    let mut per_room: Vec<Vec<GFixture>> = vec![vec![]; rooms.len()];
    let mut fits = vec![];
    for i in order {
        let g = &geos[i];
        if g.clear[2] - g.clear[0] < 0.6 || g.clear[3] - g.clear[1] < 0.6 {
            continue;
        }
        let f = facts(i);
        let mut best: Option<(Vec<String>, String, State)> = None;
        for rule in rules.rooms.iter().filter(|r| r.matches(&f)) {
            let st = State {
                taken: vec![],
                raised: vec![],
                keep: vec![],
                out: vec![],
                items: vec![],
                falls: vec![],
                turning: vec![],
                triangle: None,
                dwell: dwell.clone(),
            };
            let mut cx = Cx { g, rules, st, dwelling: dwelling_of(rooms[i].owner, typology), typology, refused: Default::default() };
            let unmet = cx.steps(&rule.steps);
            let better = best.as_ref().is_none_or(|b| unmet.len() < b.0.len());
            let done = unmet.is_empty();
            if better {
                best = Some((unmet, rule.id.clone(), cx.st));
            }
            if done {
                break;
            }
        }
        let Some((unmet, rule, st)) = best else {
            continue;
        };
        dwell.clone_from(&st.dwell);
        let mut fit = RoomFit { room: i, rule, issues: unmet, ..Default::default() };
        drains(rules, g, &st, &mut fit);
        per_room[i] = st.out;
        fits.push(fit);
    }
    fits.sort_by_key(|f| f.room);
    (per_room.into_iter().flatten().collect(), fits)
}

/// Drained pieces among `fx` (fixture, item): id, longest branch, body centre.
fn drained_of<'r, 'f>(rules: &'r Rules, fx: impl Iterator<Item = (&'f GFixture, usize)>) -> Vec<(&'r str, f64, (f64, f64))> {
    fx.filter_map(|(f, i)| {
        let it = &rules.items[i];
        let d = it.drain?;
        let b = Placed::of(it, f).body;
        Some((it.id.as_str(), d.max_run, ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0)))
    })
    .collect()
}

/// The stack of the drained `pieces`: the room corner or wet-side spot with the fewest branches past
/// the drain rules (length or drop), then the shortest longest branch; with that count.
fn stack_for(rules: &Rules, g: &Geo<'_>, pieces: &[(&str, f64, (f64, f64))]) -> Option<((f64, f64), usize)> {
    let dr = rules.drainage;
    // the room's corners, and along its wet side, where a stack in the wall chase may stand
    let mut corners: Vec<(f64, f64)> = vec![];
    let wet = g.sides[g.wet[0]];
    let along = (1..(wet.len / STACK_STEP).ceil() as usize).map(|i| wet.at(i as f64 * STACK_STEP, 0.0));
    for p in g.wet.iter().flat_map(|&k| [g.sides[k].at(0.0, 0.0), g.sides[k].at(g.sides[k].len, 0.0)]).chain(along) {
        if !corners.iter().any(|q| (q.0 - p.0).abs() < EPS && (q.1 - p.1).abs() < EPS) {
            corners.push(p);
        }
    }
    let run = |p: &(&str, f64, (f64, f64)), s: (f64, f64)| (p.2 .0 - s.0).abs() + (p.2 .1 - s.1).abs();
    let mut best: Option<((f64, f64), usize, f64)> = None;
    for s in corners {
        let over = pieces.iter().filter(|p| run(p, s) > p.1 + EPS || run(p, s) * dr.fall > dr.max_drop + EPS).count();
        let worst = pieces.iter().map(|p| run(p, s)).fold(0.0, f64::max);
        if best.is_none_or(|b| over < b.1 || (over == b.1 && worst < b.2 - EPS)) {
            best = Some((s, over, worst));
        }
    }
    best.map(|b| (b.0, b.1))
}

/// The stack at the room corner that serves the drained pieces best, each branch along the
/// walls, and the floor falls; what goes past the rules becomes an issue.
fn drains(rules: &Rules, g: &Geo<'_>, st: &State, fit: &mut RoomFit) {
    let dr = rules.drainage;
    let pieces = drained_of(rules, st.out.iter().zip(st.items.iter().map(|x| x.0)));
    for f in &st.falls {
        fit.falls.push(*f);
        if *f > dr.max_floor_drop + EPS {
            fit.issues.push(format!("floor fall drop {f:.3} m over {:.3} m", dr.max_floor_drop));
        }
    }
    fit.turning = st.turning.clone();
    fit.triangle = st.triangle;
    if pieces.is_empty() {
        return;
    }
    let Some((s, _)) = stack_for(rules, g, &pieces) else {
        return;
    };
    fit.stack = Some(s);
    for (id, max_run, at) in pieces {
        let run = (at.0 - s.0).abs() + (at.1 - s.1).abs();
        let drop = run * dr.fall;
        if run > max_run + EPS {
            fit.issues.push(format!("{id}: branch {run:.2} m over {max_run} m"));
        }
        if drop > dr.max_drop + EPS {
            fit.issues.push(format!("{id}: branch drop {drop:.3} m over {} m", dr.max_drop));
        }
        fit.drains.push((id.to_string(), run, drop));
    }
}
