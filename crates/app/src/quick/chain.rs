// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Selection of connected walls: the whole connected network (ends and T-junctions) and the chain up to a fork.
//! Pure functions over axes (segments and arcs); `segs` collects them from the editor, [`Index`] is a
//! grid of bounding boxes: traversal is ~O(N log N) instead of checking all pairs.
use crate::editor::Editor;
use ddc_geom::arc::ArcAxis;
use ddc_geom::{v2, Tol, V2};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg {
    pub id: i64,
    pub a: V2,
    pub b: V2,
    /// Half the wall thickness, m (a T-junction touches the face, not the axis).
    pub half: f64,
    /// 0 is a straight segment, otherwise the arc bulge from `a` to `b`.
    pub bulge: f64,
}

/// End-coincidence tolerance, m.
pub const TOL: f64 = 1e-3;

/// Wall axes of the current level.
pub fn segs(ed: &Editor) -> Vec<Seg> {
    ed.walls
        .iter()
        .map(|w| Seg {
            id: w.id,
            a: w.a,
            b: w.b,
            half: ed
                .types
                .iter()
                .find(|t| t.id == w.wall_type)
                .map_or(0.05, |t| t.thickness() / 2.0),
            bulge: w.bulge,
        })
        .collect()
}

fn near(p: V2, q: V2, tol: f64) -> bool {
    (p - q).len() <= tol
}

fn arc_of(s: &Seg) -> Option<ArcAxis> {
    if s.bulge == 0.0 {
        return None;
    }
    ArcAxis::new(s.a, s.b, s.bulge, Tol::default()).ok()
}

fn axis_len(s: &Seg) -> f64 {
    arc_of(s).map_or((s.b - s.a).len(), |ax| ax.length())
}

/// Distance from a point to the axis (for an arc, along the circle within the arc) and the 0..1 parameter.
fn to_seg(p: V2, s: &Seg) -> (f64, f64) {
    if let Some(ax) = arc_of(s) {
        let len = ax.length().max(1e-12);
        let u = ax.param_of(p).clamp(0.0, len);
        return ((p - ax.point_at(u)).len(), u / len);
    }
    let (a, b) = (s.a, s.b);
    let d = b - a;
    let l2 = d.x * d.x + d.y * d.y;
    let t = if l2 > 0.0 {
        (((p.x - a.x) * d.x + (p.y - a.y) * d.y) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let q = v2(a.x + d.x * t, a.y + d.y * t);
    ((p - q).len(), t)
}

/// End `p` abuts the middle of `s` (a T): on the axis or on the face, not near its ends.
fn tee(p: V2, s: &Seg, half_other: f64, tol: f64) -> bool {
    let (d, t) = to_seg(p, s);
    let len = axis_len(s).max(1e-9);
    let inner = t * len > tol && (1.0 - t) * len > tol;
    inner && d <= s.half.max(half_other) + tol
}

/// Walls are connected: a shared end, or a T-junction of one wall's end onto another.
fn linked(x: &Seg, y: &Seg, tol: f64) -> bool {
    let ends = |s: &Seg| [s.a, s.b];
    ends(x)
        .iter()
        .any(|&p| ends(y).iter().any(|&q| near(p, q, tol)))
        || ends(x).iter().any(|&p| tee(p, y, x.half, tol))
        || ends(y).iter().any(|&p| tee(p, x, y.half, tol))
}

/// Axis bounding box padded for thickness and tolerance. A minor arc lies in the chord box expanded by
/// the sagitta; a major arc lies in the circle's box.
fn bounds(s: &Seg, tol: f64) -> (V2, V2) {
    let mut pad = s.half.max(0.0) + tol;
    let (mut lo, mut hi) = (
        v2(s.a.x.min(s.b.x), s.a.y.min(s.b.y)),
        v2(s.a.x.max(s.b.x), s.a.y.max(s.b.y)),
    );
    if let Some(ax) = arc_of(s) {
        if s.bulge.abs() <= 1.0 {
            pad += s.bulge.abs() * (s.b - s.a).len() / 2.0;
        } else {
            let r = ax.radius;
            lo = v2(ax.center.x - r, ax.center.y - r);
            hi = v2(ax.center.x + r, ax.center.y + r);
        }
    }
    (v2(lo.x - pad, lo.y - pad), v2(hi.x + pad, hi.y + pad))
}

/// A box covering more cells than this goes to a shared list (very long walls on a fine grid).
const CELLS_MAX: i64 = 4096;

/// Grid of wall bounding boxes: connection candidates come only from the box's cells.
pub struct Index<'a> {
    pub segs: &'a [Seg],
    tol: f64,
    cell: f64,
    grid: HashMap<(i64, i64), Vec<usize>>,
    big: Vec<usize>,
    by_id: HashMap<i64, usize>,
}

impl<'a> Index<'a> {
    pub fn new(segs: &'a [Seg], tol: f64) -> Self {
        let mean = if segs.is_empty() {
            1.0
        } else {
            segs.iter().map(axis_len).sum::<f64>() / segs.len() as f64
        };
        let cell = if mean.is_finite() {
            mean.clamp(0.5, 20.0)
        } else {
            1.0
        };
        let mut ix = Index {
            segs,
            tol,
            cell,
            grid: HashMap::new(),
            big: vec![],
            by_id: HashMap::with_capacity(segs.len()),
        };
        for (i, s) in segs.iter().enumerate() {
            ix.by_id.entry(s.id).or_insert(i);
            match ix.cells(bounds(s, tol)) {
                Some((x0, y0, x1, y1))
                    if (x1 - x0 + 1).saturating_mul(y1 - y0 + 1) <= CELLS_MAX =>
                {
                    for x in x0..=x1 {
                        for y in y0..=y1 {
                            ix.grid.entry((x, y)).or_default().push(i);
                        }
                    }
                }
                _ => ix.big.push(i),
            }
        }
        ix
    }

    fn cells(&self, (lo, hi): (V2, V2)) -> Option<(i64, i64, i64, i64)> {
        let f = |v: f64| {
            let c = (v / self.cell).floor();
            (c.is_finite() && c.abs() < 1e12).then_some(c as i64)
        };
        Some((f(lo.x)?, f(lo.y)?, f(hi.x)?, f(hi.y)?))
    }

    /// Walls whose box may touch box `b`; each once (marked via `stamp`).
    fn around(&self, b: (V2, V2), mark: &mut [u32], stamp: u32, out: &mut Vec<usize>) {
        out.clear();
        let mut push = |i: usize, out: &mut Vec<usize>| {
            if mark[i] != stamp {
                mark[i] = stamp;
                out.push(i);
            }
        };
        match self.cells(b) {
            Some((x0, y0, x1, y1)) if (x1 - x0 + 1).saturating_mul(y1 - y0 + 1) <= CELLS_MAX => {
                for x in x0..=x1 {
                    for y in y0..=y1 {
                        for &i in self.grid.get(&(x, y)).into_iter().flatten() {
                            push(i, out);
                        }
                    }
                }
                for &i in &self.big {
                    push(i, out);
                }
            }
            _ => {
                for i in 0..self.segs.len() {
                    push(i, out);
                }
            }
        }
        out.sort_unstable();
    }

    /// The whole connected wall network from `start` (inclusive), in breadth-first order.
    pub fn connected(&self, start: i64) -> Vec<i64> {
        let Some(&s0) = self.by_id.get(&start) else {
            return vec![];
        };
        let n = self.segs.len();
        let (mut seen, mut mark, mut cand) = (vec![false; n], vec![0u32; n], vec![]);
        seen[s0] = true;
        let mut queue = vec![s0];
        let mut i = 0;
        while i < queue.len() {
            let cur = &self.segs[queue[i]];
            self.around(bounds(cur, self.tol), &mut mark, i as u32 + 1, &mut cand);
            for &j in &cand {
                if !seen[j] && linked(cur, &self.segs[j], self.tol) {
                    seen[j] = true;
                    queue.push(j);
                }
            }
            i += 1;
        }
        queue.into_iter().map(|k| self.segs[k].id).collect()
    }

    /// Walls meeting with an end at point `p` (except `except`), and whether a T-junction is there.
    fn at_end(&self, p: V2, except: i64, mark: &mut [u32], stamp: u32) -> (Vec<(i64, V2)>, bool) {
        let t = self.tol;
        let mut cand = vec![];
        self.around(
            (v2(p.x - t, p.y - t), v2(p.x + t, p.y + t)),
            mark,
            stamp,
            &mut cand,
        );
        let mut ends = vec![];
        let mut branch = false;
        for s in cand
            .iter()
            .map(|&j| &self.segs[j])
            .filter(|s| s.id != except)
        {
            if near(s.a, p, t) {
                ends.push((s.id, s.b));
            } else if near(s.b, p, t) {
                ends.push((s.id, s.a));
            } else if tee(p, s, 0.0, t) {
                branch = true;
            }
        }
        (ends, branch)
    }

    /// Chain from `start` through simple end-to-end joints in both directions; stops at a fork
    /// (3+ ends or a T-junction) and when it closes.
    pub fn run(&self, start: i64) -> Vec<i64> {
        let Some(&k0) = self.by_id.get(&start) else {
            return vec![];
        };
        let s0 = self.segs[k0];
        let mut mark = vec![0u32; self.segs.len()];
        let mut stamp = 0u32;
        let mut out = vec![start];
        let mut inside: HashSet<i64> = HashSet::from([start]);
        for from in [s0.b, s0.a] {
            let (mut cur, mut p) = (start, from);
            loop {
                stamp += 1;
                let (ends, branch) = self.at_end(p, cur, &mut mark, stamp);
                if branch || ends.len() != 1 {
                    break;
                }
                let (next, far) = ends[0];
                if !inside.insert(next) {
                    break;
                }
                out.push(next);
                cur = next;
                p = far;
            }
        }
        out
    }
}

/// [`Index::connected`] for a single query.
pub fn connected(segs: &[Seg], start: i64, tol: f64) -> Vec<i64> {
    Index::new(segs, tol).connected(start)
}

/// [`Index::run`] for a single query.
pub fn run(segs: &[Seg], start: i64, tol: f64) -> Vec<i64> {
    Index::new(segs, tol).run(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: i64, a: (f64, f64), b: (f64, f64)) -> Seg {
        Seg {
            id,
            a: v2(a.0, a.1),
            b: v2(b.0, b.1),
            half: 0.1,
            bulge: 0.0,
        }
    }

    #[test]
    fn chain_selection_connected_and_run() {
        // 1-2-3 polyline, 4 abuts the middle of 2 as a T, 5 starts at the end of 4, 6 is separate,
        // 7 crosses 1 as an X with no shared points
        let w = vec![
            s(1, (0.0, 0.0), (4.0, 0.0)),
            s(2, (4.0, 0.0), (4.0, 4.0)),
            s(3, (4.0, 4.0), (8.0, 4.0)),
            s(4, (4.1, 2.0), (7.0, 2.0)),
            s(5, (7.0, 2.0), (7.0, 0.5)),
            s(6, (10.0, 0.0), (12.0, 0.0)),
            s(7, (2.0, -1.0), (2.0, 1.0)),
        ];
        let mut c = connected(&w, 1, TOL);
        c.sort();
        assert_eq!(c, vec![1, 2, 3, 4, 5]);
        assert_eq!(connected(&w, 6, TOL), vec![6]);
        assert_eq!(connected(&w, 99, TOL), Vec::<i64>::new());
        // chain: 1→2→3 with no branch (the T on 2 is mid-wall, not an end: not a chain fork)
        let mut r = run(&w, 1, TOL);
        r.sort();
        assert_eq!(r, vec![1, 2, 3]);
        let mut r = run(&w, 5, TOL);
        r.sort();
        // 4 starts with a T-junction onto 2: the 5→4 chain stops there
        assert_eq!(r, vec![4, 5]);
        // a fork of three ends at one point stops the chain
        let y = vec![
            s(1, (0.0, 0.0), (1.0, 0.0)),
            s(2, (1.0, 0.0), (2.0, 0.0)),
            s(3, (1.0, 0.0), (1.0, 1.0)),
        ];
        assert_eq!(run(&y, 1, TOL), vec![1]);
        let mut c = connected(&y, 1, TOL);
        c.sort();
        assert_eq!(c, vec![1, 2, 3]);
        // a closed loop is traversed fully and does not cycle forever
        let sq = vec![
            s(1, (0.0, 0.0), (1.0, 0.0)),
            s(2, (1.0, 0.0), (1.0, 1.0)),
            s(3, (1.0, 1.0), (0.0, 1.0)),
            s(4, (0.0, 1.0), (0.0, 0.0)),
        ];
        let mut r = run(&sq, 2, TOL);
        r.sort();
        assert_eq!(r, vec![1, 2, 3, 4]);
    }

    #[test]
    fn arcs_by_true_geometry() {
        // semicircle (0,0)→(4,0), bulge 1: center (2,0), r 2, arc midpoint (2,-2)
        let mut arc = s(1, (0.0, 0.0), (4.0, 0.0));
        arc.bulge = 1.0;
        let on_arc = s(2, (2.0, -2.0), (2.0, -4.0));
        let on_chord = s(3, (2.0, 0.0), (2.0, 1.0));
        let w = vec![arc, on_arc, on_chord];
        let mut c = connected(&w, 1, TOL);
        c.sort();
        assert_eq!(c, vec![1, 2], "a T-junction at the arc middle connects, one at the chord does not");
        // arc end to a straight segment's end continues the chain
        let w = vec![arc, s(4, (4.0, 0.0), (6.0, 0.0)), on_arc];
        let mut r = run(&w, 4, TOL);
        r.sort();
        assert_eq!(r, vec![1, 4]);
    }

    #[test]
    fn big_grid_is_fast_and_exact() {
        // 40x40 grid of 3 m cells: 2·40·41 = 3280 walls, all connected; the separate one is not
        let n = 40;
        let mut w = vec![];
        let mut id = 0;
        for i in 0..=n {
            for j in 0..n {
                let (x, y) = (i as f64 * 3.0, j as f64 * 3.0);
                id += 1;
                w.push(s(id, (x, y), (x, y + 3.0)));
                id += 1;
                w.push(s(id, (y, x), (y + 3.0, x)));
            }
        }
        w.push(s(-1, (500.0, 0.0), (503.0, 0.0)));
        let t = web_time::Instant::now();
        let ix = Index::new(&w, TOL);
        let c = ix.connected(1);
        assert_eq!(c.len(), w.len() - 1);
        assert_eq!(
            c.iter().collect::<HashSet<_>>().len(),
            c.len(),
            "no duplicates"
        );
        assert_eq!(ix.connected(-1), vec![-1]);
        assert_eq!(
            ix.run(1),
            vec![1, 2],
            "a grid corner continues the chain, a fork stops it"
        );
        assert!(t.elapsed().as_secs_f64() < 2.0, "{:?}", t.elapsed());
    }

    #[test]
    fn chain_on_editor_walls() {
        use crate::wall_tool::{Event, Mode};
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Press(v2(0.0, 0.0)));
        ed.handle(Event::Release(v2(6.0, 4.0)));
        ed.handle(Event::Press(v2(10.0, 0.0)));
        ed.handle(Event::Release(v2(12.0, 2.0)));
        ed.set_mode(Mode::Select);
        assert_eq!(ed.walls.len(), 8);
        let sg = segs(&ed);
        let first = ed.walls[0].id;
        assert_eq!(connected(&sg, first, TOL).len(), 4);
        assert_eq!(run(&sg, first, TOL).len(), 4);
    }
}
