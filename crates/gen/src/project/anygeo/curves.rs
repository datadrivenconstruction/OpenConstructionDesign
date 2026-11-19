// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Curved facade runs. A plate outline keeps its arcs and splines as chords within the arc
//! tolerance; the exterior walls along such a run are one curved wall. A run is a stretch of
//! the facade whose every bend is gentle; it is an arc when all its points lie on one circle,
//! else a cubic through its bends. A closed curve (a round or elliptical tower) is two runs.

use super::{cross, dist, dot, sub, P};
use crate::project::rules::rule;
use crate::{GLayout, WallRole};
use std::collections::BTreeMap;

/// Largest turn between two chords that still continues one curve, degrees. Arcs cut at the
/// tolerance turn far less; a 24-sided polygon turns 15 degrees and stays a polygon.
const SMOOTH_DEG: f64 = 14.0;
/// Turns below this are a straight facade cut by a room wall, degrees.
const STRAIGHT_DEG: f64 = 0.2;
/// A curve bends at this many vertices at least.
const MIN_BENDS: usize = 2;
/// A circle wider than this is a straight line, m.
const MAX_RADIUS: f64 = 10_000.0;

/// One curved wall: the straight exterior walls that stand for it, in order along the run,
/// and its axis. `arc` is the exact circle (centre, radius) when the run is circular; else
/// the wall follows a cubic through `through`. Hosted openings stay on the straight walls.
#[derive(Clone, Debug, PartialEq)]
pub struct GCurve {
    pub walls: Vec<usize>,
    /// Axis points from start to end: the run's ends and every bend.
    pub through: Vec<P>,
    pub arc: Option<(P, f64)>,
}

/// A smooth stretch of an outline.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Run {
    /// Points from start to end; a closed run repeats its first point at the end.
    pub pts: Vec<P>,
    pub len: f64,
    pub closed: bool,
    pub arc: Option<(P, f64)>,
}

/// Signed turn at `b`, degrees, positive to the left.
fn turn(a: P, b: P, c: P) -> f64 {
    let (u, v) = (sub(b, a), sub(c, b));
    cross(u, v).atan2(dot(u, v)).to_degrees()
}

/// Circle through three points.
pub(crate) fn circle3(a: P, b: P, c: P) -> Option<(P, f64)> {
    let d = 2.0 * (a.0 * (b.1 - c.1) + b.0 * (c.1 - a.1) + c.0 * (a.1 - b.1));
    if d.abs() < 1e-12 {
        return None;
    }
    let (a2, b2, c2) = (dot(a, a), dot(b, b), dot(c, c));
    let ux = (a2 * (b.1 - c.1) + b2 * (c.1 - a.1) + c2 * (a.1 - b.1)) / d;
    let uy = (a2 * (c.0 - b.0) + b2 * (a.0 - c.0) + c2 * (b.0 - a.0)) / d;
    let r = dist((ux, uy), a);
    (r.is_finite() && r < MAX_RADIUS).then_some(((ux, uy), r))
}

/// The circle every point of `pts` lies on, within twice the arc tolerance.
fn fit(pts: &[P]) -> Option<(P, f64)> {
    let m = pts.len();
    if m < 3 {
        return None;
    }
    let (i1, i2) = if dist(pts[0], pts[m - 1]) < 1e-6 { (m / 3, 2 * m / 3) } else { (m / 2, m - 1) };
    let (c, r) = circle3(pts[0], pts[i1], pts[i2])?;
    let tol = 2.0 * rule("massing.arc_tolerance") + 1e-3;
    pts.iter().all(|p| (dist(*p, c) - r).abs() <= tol).then_some((c, r))
}

/// Edge ranges `[e0, e1)` of the smooth runs of a polyline; edge `e` joins point `e` to the
/// next (indices past the end wrap round a closed ring). A closed ring that is smooth all the
/// way round is the one range `(0, n)`.
fn edge_runs(pts: &[P], closed: bool) -> Vec<(usize, usize)> {
    let n = pts.len();
    let edges = if closed { n } else { n.saturating_sub(1) };
    if n < 3 || edges < 2 {
        return vec![];
    }
    let tn = |i: usize| turn(pts[(i + n - 1) % n], pts[i % n], pts[(i + 1) % n]);
    let smooth = |i: usize| tn(i).abs() <= SMOOTH_DEG;
    let mut out = vec![];
    if closed {
        let Some(s) = (0..n).find(|&i| !smooth(i)) else {
            return if (0..n).filter(|&i| tn(i).abs() > STRAIGHT_DEG).count() >= MIN_BENDS { vec![(0, n)] } else { vec![] };
        };
        let mut e0 = s;
        for k in 1..=n {
            let v = s + k;
            if k == n || !smooth(v % n) {
                out.push((e0, v));
                e0 = v;
            }
        }
    } else {
        let mut e0 = 0;
        for v in 1..n {
            if v == n - 1 || !smooth(v) {
                out.push((e0, v));
                e0 = v;
            }
        }
    }
    out.retain(|&(e0, e1)| (e0 + 1..e1).filter(|&v| tn(v).abs() > STRAIGHT_DEG).count() >= MIN_BENDS);
    out
}

/// The smooth runs of an outline ring (`closed`) or of an open polyline.
pub(crate) fn smooth_runs(ring: &[P], closed: bool) -> Vec<Run> {
    let n = ring.len();
    edge_runs(ring, closed)
        .into_iter()
        .map(|(e0, e1)| {
            let pts: Vec<P> = (e0..=e1).map(|v| ring[v % n]).collect();
            let len = pts.windows(2).map(|w| dist(w[0], w[1])).sum();
            Run { arc: fit(&pts), closed: closed && e1 - e0 == n, pts, len }
        })
        .collect()
}

fn key(p: P) -> (i64, i64) {
    ((p.0 * 1e4).round() as i64, (p.1 * 1e4).round() as i64)
}

/// The exterior walls joined end to end: each chain as (wall, start, end) in order, and
/// whether it closes. A wall the generator already made an arc is no chord and ends a chain.
fn chains(l: &GLayout) -> Vec<(Vec<(usize, P, P)>, bool)> {
    let ext: Vec<usize> =
        (0..l.walls.len()).filter(|&i| l.walls[i].role == WallRole::Exterior && l.walls[i].bulge == 0.0).collect();
    let mut at: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
    for &i in &ext {
        at.entry(key(l.walls[i].a)).or_default().push(i);
        at.entry(key(l.walls[i].b)).or_default().push(i);
    }
    let mut used = vec![false; l.walls.len()];
    let mut out = vec![];
    let other = |i: usize, p: P| if key(l.walls[i].a) == key(p) { l.walls[i].b } else { l.walls[i].a };
    for &w0 in &ext {
        if used[w0] {
            continue;
        }
        used[w0] = true;
        let mut chain = vec![(w0, l.walls[w0].a, l.walls[w0].b)];
        // forward from the end, then backward from the start
        let mut cur = l.walls[w0].b;
        while let Some(&w) = at.get(&key(cur)).and_then(|v| v.iter().find(|&&w| !used[w])) {
            used[w] = true;
            let next = other(w, cur);
            chain.push((w, cur, next));
            cur = next;
        }
        let closed = key(cur) == key(l.walls[w0].a);
        if !closed {
            let mut cur = l.walls[w0].a;
            while let Some(&w) = at.get(&key(cur)).and_then(|v| v.iter().find(|&&w| !used[w])) {
                used[w] = true;
                let prev = other(w, cur);
                chain.insert(0, (w, prev, cur));
                cur = prev;
            }
        }
        out.push((chain, closed));
    }
    out
}

fn curve(chain: &[(usize, P, P)], e0: usize, e1: usize) -> Option<GCurve> {
    let n = chain.len();
    let walls: Vec<usize> = (e0..e1).map(|e| chain[e % n].0).collect();
    let mut pts: Vec<P> = (e0..e1).map(|e| chain[e % n].1).collect();
    pts.push(chain[(e1 + n - 1) % n].2);
    if walls.len() < 2 {
        return None;
    }
    let m = pts.len();
    let through: Vec<P> = (0..m)
        .filter(|&i| i == 0 || i == m - 1 || turn(pts[i - 1], pts[i], pts[i + 1]).abs() > STRAIGHT_DEG)
        .map(|i| pts[i])
        .collect();
    (through.len() >= 3).then(|| GCurve { walls, arc: fit(&through), through })
}

/// The curved walls of a storey: every smooth run of its exterior walls. Pure; the walls
/// themselves stay straight, so a reader without curved walls loses nothing.
pub fn curve_walls(l: &GLayout) -> Vec<GCurve> {
    let mut out = vec![];
    for (chain, closed) in chains(l) {
        let pts: Vec<P> = chain.iter().map(|c| c.1).chain((!closed).then(|| chain[chain.len() - 1].2)).collect();
        let n = chain.len();
        for (e0, e1) in edge_runs(&pts, closed) {
            if closed && e1 - e0 == n {
                // a closed curve: two halves, each an open wall
                out.extend(curve(&chain, 0, n / 2));
                out.extend(curve(&chain, n / 2, n));
            } else {
                out.extend(curve(&chain, e0, e1));
            }
        }
    }
    out
}
