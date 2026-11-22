// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The clean-up pass over the finished runs. Each movable run is straightened on its own:
//! straight joints merged, detours, jogs and short straights between fittings cut out where a
//! straight or one corner does instead, offsets snapped to the setting-out grid, and a run
//! crossing a system above it in the void order moved under it. A change is kept only when
//! every run end that met the run still meets it, no run end meets it anew, every straight
//! still holds its fittings and the run's boxes touch nothing they did not touch before. The
//! whole project is then checked as the model checks it, and a run that brought a new fault
//! goes back to how its system routed it.

use std::collections::{BTreeMap, BTreeSet};

use super::metrics::{bends, is_bend, level, min_seg, norm, short_segs, sub, turn};
use super::segs::{lines, points_mut, void_id, Kind, Line, Src};
use super::super::rules::rule;
use super::super::svc;
use super::super::GProject;

type P3 = (f64, f64, f64);

/// Joint tolerance, as the model's.
const TOL: f64 = 1e-3;
const CELL: f64 = 2.0;

pub(super) fn cells(b: &[f64; 6]) -> impl Iterator<Item = (i64, i64)> {
    let (x0, x1) = ((b[0] / CELL).floor() as i64, (b[3] / CELL).floor() as i64);
    let (y0, y1) = ((b[1] / CELL).floor() as i64, (b[4] / CELL).floor() as i64);
    (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| (x, y)))
}

fn end_cell(q: P3) -> (i64, i64) {
    ((q.0 / CELL).floor() as i64, (q.1 / CELL).floor() as i64)
}

fn dist_seg(p: P3, a: P3, b: P3) -> f64 {
    let d = sub(b, a);
    let l2 = d.0 * d.0 + d.1 * d.1 + d.2 * d.2;
    let t = if l2 > 0.0 { ((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1 + (p.2 - a.2) * d.2) / l2 } else { 0.0 };
    let t = t.clamp(0.0, 1.0);
    norm(sub(p, (a.0 + d.0 * t, a.1 + d.1 * t, a.2 + d.2 * t)))
}

fn dist_poly(p: P3, q: &[P3]) -> f64 {
    q.windows(2).map(|w| dist_seg(p, w[0], w[1])).fold(f64::INFINITY, f64::min)
}

/// Systems whose run ends join: the power runs are one system.
fn group(l: &Line) -> &str {
    if l.key.starts_with("power") {
        "power"
    } else {
        &l.key
    }
}

/// Box of a straight piece with its outer size across the axis, as the services check takes it.
fn seg_box(size: (f64, f64), a: P3, b: P3) -> [f64; 6] {
    let (w, h) = size;
    let d = sub(b, a);
    let plan = d.0.hypot(d.1);
    let (hx, hy, hz) = if d.2.abs() > plan {
        (w / 2.0, h / 2.0, 0.0)
    } else if d.0.abs() >= d.1.abs() {
        (0.0, w / 2.0, h / 2.0)
    } else {
        (w / 2.0, 0.0, h / 2.0)
    };
    [a.0.min(b.0) - hx, a.1.min(b.1) - hy, a.2.min(b.2) - hz, a.0.max(b.0) + hx, a.1.max(b.1) + hy, a.2.max(b.2) + hz]
}

fn overlap(a: &[f64; 6], b: &[f64; 6], gap: f64) -> bool {
    (0..3).all(|i| a[i] < b[i + 3] + gap - 5e-4 && b[i] < a[i + 3] + gap - 5e-4)
}

/// What a piece touches: another line, an obstacle, a slab.
/// What a polyline meets, with the number of segment pairs for each.
type Hits = BTreeMap<Hit, usize>;

/// Whether `a` meets nothing `b` does not, and nothing more often.
fn within(a: &Hits, b: &Hits) -> bool {
    a.iter().all(|(k, n)| b.get(k).is_some_and(|m| n <= m))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Hit {
    Line(usize),
    Obstacle(usize),
    Slab(usize),
}

pub(super) struct World {
    pub ls: Vec<Line>,
    /// Segment boxes: line, ends, box; a changed line's old pieces are dropped.
    pub segs: Vec<(usize, P3, P3, [f64; 6])>,
    pub alive: Vec<bool>,
    /// Segment ids of each line.
    of: Vec<Vec<usize>>,
    pub grid: BTreeMap<(i64, i64), Vec<usize>>,
    /// Run ends by plan cell: line and point.
    ends: BTreeMap<(i64, i64), Vec<(usize, P3)>>,
    obst: Vec<([f64; 6], &'static str)>,
    obst_grid: BTreeMap<(i64, i64), Vec<usize>>,
    /// Slab bands of the storeys: bottom and top.
    slabs: Vec<(f64, f64)>,
    radius: Vec<f64>,
    /// Lines moved together by a slide: they go back together.
    pub bonds: Vec<(usize, usize)>,
}

impl World {
    fn new(p: &GProject) -> World {
        let ls = lines(p);
        let radius = ls.iter().map(|l| bend_radius(p, l)).collect();
        let slabs = p.storeys.iter().map(|s| (s.elevation - svc::slab(), s.elevation)).collect();
        let mut w = World {
            ls,
            segs: vec![],
            alive: vec![],
            of: vec![],
            grid: BTreeMap::new(),
            ends: BTreeMap::new(),
            obst: svc::obstacles_with(p, false),
            obst_grid: BTreeMap::new(),
            slabs,
            radius,
            bonds: vec![],
        };
        for (k, o) in w.obst.iter().enumerate() {
            for c in cells(&o.0) {
                w.obst_grid.entry(c).or_default().push(k);
            }
        }
        w.of = vec![vec![]; w.ls.len()];
        for i in 0..w.ls.len() {
            w.index(i);
            if let (Some(&a), Some(&b)) = (w.ls[i].pts.first(), w.ls[i].pts.last()) {
                for q in [a, b] {
                    w.ends.entry(end_cell(q)).or_default().push((i, q));
                }
            }
        }
        w
    }

    fn index(&mut self, i: usize) {
        let l = &self.ls[i];
        for s in l.pts.windows(2) {
            let b = seg_box(l.size, s[0], s[1]);
            let id = self.segs.len();
            self.segs.push((i, s[0], s[1], b));
            self.alive.push(true);
            self.of[i].push(id);
            for c in cells(&b) {
                self.grid.entry(c).or_default().push(id);
            }
        }
    }

    fn set(&mut self, i: usize, pts: Vec<P3>) {
        for k in std::mem::take(&mut self.of[i]) {
            self.alive[k] = false;
        }
        let old = (self.ls[i].pts[0], *self.ls[i].pts.last().unwrap());
        let new = (pts[0], *pts.last().unwrap());
        self.ls[i].pts = pts;
        self.index(i);
        if old != new {
            for q in [old.0, old.1] {
                if let Some(v) = self.ends.get_mut(&end_cell(q)) {
                    v.retain(|e| e.0 != i);
                }
            }
            for q in [new.0, new.1] {
                self.ends.entry(end_cell(q)).or_default().push((i, q));
            }
        }
    }

    /// Run ends of the same system lying on the polyline, the line's own ends aside.
    pub(super) fn touching(&self, i: usize, q: &[P3]) -> BTreeSet<(usize, u8)> {
        let g = group(&self.ls[i]);
        let mut out = BTreeSet::new();
        for w in q.windows(2) {
            let mut b = seg_box((0.0, 0.0), w[0], w[1]);
            b.iter_mut().take(3).for_each(|v| *v -= TOL);
            b.iter_mut().skip(3).for_each(|v| *v += TOL);
            for c in cells(&b) {
                for &(j, e) in self.ends.get(&c).into_iter().flatten() {
                    if j != i && group(&self.ls[j]) == g && dist_seg(e, w[0], w[1]) < TOL {
                        out.insert((j, u8::from(self.ls[j].pts[0] != e)));
                    }
                }
            }
        }
        out
    }

    /// Whatever the boxes of the polyline meet.
    fn hits(&self, i: usize, q: &[P3]) -> Hits {
        let l = &self.ls[i];
        let g = group(l);
        let sys = match l.key.as_str() {
            "cold" => "Cold",
            "hot" => "Hot",
            "sanitary" => "Sanitary",
            k if k.starts_with("power") => "Power",
            k if k.starts_with("duct") => "duct",
            _ => "",
        };
        let mut out = Hits::new();
        for w in q.windows(2) {
            let b = seg_box(l.size, w[0], w[1]);
            for c in cells(&b) {
                for &k in self.obst_grid.get(&c).into_iter().flatten() {
                    let o = &self.obst[k];
                    if overlap(&o.0, &b, 0.0) && !svc::shaft_takes(o.1, sys) {
                        *out.entry(Hit::Obstacle(k)).or_default() += 1;
                    }
                }
            }
            let upright = (w[0].0 - w[1].0).abs() < 1e-9 && (w[0].1 - w[1].1).abs() < 1e-9;
            if !upright {
                for (k, s) in self.slabs.iter().enumerate() {
                    if b[5] > s.0 + 2e-3 && b[2] < s.1 - 2e-3 {
                        *out.entry(Hit::Slab(k)).or_default() += 1;
                    }
                }
            }
            let mut seen = BTreeSet::new();
            for c in cells(&b) {
                for &id in self.grid.get(&c).into_iter().flatten() {
                    if !self.alive[id] || !seen.insert(id) {
                        continue;
                    }
                    let (j, a2, b2, bb) = self.segs[id];
                    if j == i {
                        continue;
                    }
                    let o = &self.ls[j];
                    let same = group(o) == g;
                    let gap = if same { 0.0 } else { clear(l, o) };
                    if !overlap(&bb, &b, gap) {
                        continue;
                    }
                    let near = |p: P3, a: P3, z: P3| dist_seg(p, a, z) < 2e-3;
                    let joined = same && (near(a2, w[0], w[1]) || near(b2, w[0], w[1]) || near(w[0], a2, b2) || near(w[1], a2, b2));
                    if !joined {
                        *out.entry(Hit::Line(j)).or_default() += 1;
                    }
                }
            }
        }
        out
    }
}

/// Clear gap kept between two lines of different systems.
fn clear(a: &Line, b: &Line) -> f64 {
    let label = |l: &Line| match l.kind {
        Kind::Duct => "duct",
        Kind::Tray | Kind::Conduit => "Power",
        Kind::Pipe => "Cold",
    };
    super::metrics::clearance(label(a), label(b))
}

fn bend_radius(p: &GProject, l: &Line) -> f64 {
    match l.src {
        Src::Water(i) => svc::bend_max(&p.services.water[i]),
        Src::Elec(i) => svc::bend_max(&p.services.elec[i]),
        Src::Fire(i) => svc::bend_max(&p.fire.runs[i].run),
        Src::Stand(s, i) => svc::bend_max(&p.fire.standpipes[s].runs[i].run),
        Src::Duct(i) => {
            let d = &p.hvac.ducts[i];
            d.bend.unwrap_or_else(|| {
                let sec = svc::duct_section(d.size);
                ddc_geom::mep::default_bend_radius(&sec, true).max(ddc_geom::mep::default_bend_radius(&sec, false))
            })
        }
    }
}

/// Whether every straight holds the elbows at its ends; the run's own ends take none.
fn fits(q: &[P3], r: f64) -> bool {
    let n = q.len();
    let trim = |k: usize| {
        if k == 0 || k + 1 >= n {
            return 0.0;
        }
        let t = turn(q[k - 1], q[k], q[k + 1]).to_radians();
        if t < 1e-6 {
            0.0
        } else {
            r * (t / 2.0).tan()
        }
    };
    (0..n - 1).all(|k| norm(sub(q[k + 1], q[k])) + 1e-6 >= trim(k) + trim(k + 1))
}

/// Drops points that add nothing: repeated ones and straight-through ones.
fn tidy(q: Vec<P3>) -> Vec<P3> {
    let mut out: Vec<P3> = vec![];
    for p in q {
        if out.last().is_some_and(|l| norm(sub(p, *l)) < 1e-6) {
            continue;
        }
        while out.len() >= 2 && turn(out[out.len() - 2], out[out.len() - 1], p) < 0.05 {
            out.pop();
        }
        out.push(p);
    }
    out
}

fn length(p: &[P3]) -> f64 {
    p.windows(2).map(|w| norm(sub(w[1], w[0]))).sum()
}

/// What the pass lowers, in order: bends and short straights, then length.
fn score(q: &[P3], kind: Kind) -> (usize, i64) {
    (bends(q) + 2 * short_segs(q, kind), (length(q) * 1e4).round() as i64)
}

/// Whether a changed polyline may replace the line's own.
fn admissible(w: &World, i: usize, q: &[P3]) -> bool {
    let l = &w.ls[i];
    if q.first() != l.pts.first() || q.last() != l.pts.last() || !fits(q, w.radius[i]) {
        return false;
    }
    if w.touching(i, q) != w.touching(i, &l.pts) {
        return false;
    }
    within(&w.hits(i, q), &w.hits(i, &l.pts))
}

/// How far a removed point may lie off the new path: two sizes, at least the least straight.
fn reach(l: &Line) -> f64 {
    let s = l.size.0.max(l.size.1);
    match l.kind {
        Kind::Conduit => 2.0 * s,
        _ => (2.0 * s).max(min_seg(l.kind)),
    }
}

/// Paths from `a` to `b` in their level plane with at most one corner.
fn shortcuts(a: P3, b: P3) -> Vec<Vec<P3>> {
    let (dx, dy) = ((b.0 - a.0).abs(), (b.1 - a.1).abs());
    if dx < 1e-6 || dy < 1e-6 {
        vec![vec![a, b]]
    } else {
        vec![vec![a, (b.0, a.1, a.2), b], vec![a, (a.0, b.1, a.2), b]]
    }
}

/// The best single shortcut of a window of level points, if one scores better.
fn best_shortcut(w: &World, i: usize) -> Option<Vec<P3>> {
    let l = &w.ls[i];
    let p = &l.pts;
    let n = p.len();
    let base = score(p, l.kind);
    let lim = reach(l);
    let mut best: Option<((usize, i64), Vec<P3>)> = None;
    for a in 0..n.saturating_sub(2) {
        for b in a + 2..n {
            let flat = p[a..=b].iter().all(|q| (q.2 - p[a].2).abs() < 1e-6);
            if !flat {
                break;
            }
            for cut in shortcuts(p[a], p[b]) {
                if p[a + 1..b].iter().any(|&q| dist_poly(q, &cut) > lim) {
                    continue;
                }
                let mut q: Vec<P3> = p[..a].to_vec();
                q.extend(&cut);
                q.extend(&p[b + 1..]);
                let q = tidy(q);
                let s = score(&q, l.kind);
                if s.1 > base.1 + 10 || s >= base || best.as_ref().is_some_and(|b| s >= b.0) {
                    continue;
                }
                if admissible(w, i, &q) {
                    best = Some((s, q));
                }
            }
        }
    }
    best.map(|b| b.1)
}

/// Level interior straights between two legs across them move onto the grid, with the run
/// ends on them; returns whether one moved.
fn snap(w: &mut World, i: usize, origin: (f64, f64)) -> bool {
    let s = rule("mep.coord.snap");
    let mut moved = false;
    let mut k = 1;
    while k + 2 < w.ls[i].pts.len() {
        let q = &w.ls[i].pts;
        let (a, b) = (q[k], q[k + 1]);
        let along_x = (a.1 - b.1).abs() < 1e-6 && (a.2 - b.2).abs() < 1e-6;
        let along_y = (a.0 - b.0).abs() < 1e-6 && (a.2 - b.2).abs() < 1e-6;
        k += 1;
        if along_x == along_y {
            continue;
        }
        let (c, o) = if along_x { (a.1, origin.1) } else { (a.0, origin.0) };
        let t = o + ((c - o) / s).round() * s;
        if (t - c).abs() < 1e-6 {
            continue;
        }
        // a U-shaped stretch grows when it moves out: only Z-shaped ones or inward moves
        let (u, v) = if along_x { (q[k - 2].1, q[k + 1].1) } else { (q[k - 2].0, q[k + 1].0) };
        if (u - t).abs() + (t - v).abs() > (u - c).abs() + (c - v).abs() + 1e-9 {
            continue;
        }
        let d = if along_x { (0.0, t - c, 0.0) } else { (t - c, 0.0, 0.0) };
        moved |= slide(w, i, k - 1, k, d);
    }
    moved
}

fn add(a: P3, b: P3) -> P3 {
    (a.0 + b.0, a.1 + b.1, a.2 + b.2)
}

fn parallel(a: P3, b: P3, d: P3) -> bool {
    let v = sub(b, a);
    let (lv, ld) = (norm(v), norm(d));
    lv > 1e-9 && ld > 1e-9 && ((v.0 * d.0 + v.1 * d.1 + v.2 * d.2).abs() / (lv * ld)) > 1.0 - 1e-9
}

/// Moves the points `k..=m` of line `i` by `d`: the legs before and after run along `d` and
/// stretch, and every run end of the system on the moved part follows on its own last leg
/// along `d`. Kept only when each moved line keeps its joints and fittings and touches
/// nothing new; returns whether it was.
pub(super) fn slide(w: &mut World, i: usize, k: usize, m: usize, d: P3) -> bool {
    let p = w.ls[i].pts.clone();
    let n = p.len();
    if w.ls[i].fixed() || k < 1 || k > m || m + 2 > n || norm(d) < 1e-9 {
        return false;
    }
    if !parallel(p[k - 1], p[k], d) || !parallel(p[m], p[m + 1], d) {
        return false;
    }
    let mut q = p.clone();
    for x in q.iter_mut().take(m + 1).skip(k) {
        *x = add(*x, d);
    }
    let keeps = |a0: P3, a1: P3, b0: P3, b1: P3| {
        let (u, v) = (sub(a1, a0), sub(b1, b0));
        u.0 * v.0 + u.1 * v.1 + u.2 * v.2 > 0.0 && norm(v) > 1e-3
    };
    if !keeps(p[k - 1], p[k], q[k - 1], q[k]) || !keeps(p[m], p[m + 1], q[m], q[m + 1]) {
        return false;
    }
    let chain = &p[k..=m];
    let on = |e: P3| if chain.len() == 1 { norm(sub(e, chain[0])) < TOL } else { dist_poly(e, chain) < TOL };
    let g = group(&w.ls[i]).to_string();
    let mut movers: Vec<(usize, Vec<P3>)> = vec![(i, q)];
    let mut cand: BTreeSet<usize> = BTreeSet::new();
    for x in p.iter().take(m + 1).skip(k) {
        let c = end_cell(*x);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for &(j, _) in w.ends.get(&(c.0 + dx, c.1 + dy)).into_iter().flatten() {
                    cand.insert(j);
                }
            }
        }
    }
    for w2 in chain.windows(2) {
        let mut b = seg_box((0.0, 0.0), w2[0], w2[1]);
        b.iter_mut().take(3).for_each(|v| *v -= TOL);
        b.iter_mut().skip(3).for_each(|v| *v += TOL);
        for c in cells(&b) {
            for &(j, _) in w.ends.get(&c).into_iter().flatten() {
                cand.insert(j);
            }
        }
    }
    for j in cand {
        if j == i || group(&w.ls[j]) != g {
            continue;
        }
        let pj = w.ls[j].pts.clone();
        let nj = pj.len();
        let (a, b) = (on(pj[0]), on(pj[nj - 1]));
        if !a && !b {
            continue;
        }
        if (a && b) || w.ls[j].fixed() || nj < 2 {
            return false;
        }
        let (e, nb) = if a { (0, 1) } else { (nj - 1, nj - 2) };
        if !parallel(pj[nb], pj[e], d) {
            return false;
        }
        let mut r = pj.clone();
        r[e] = add(r[e], d);
        if !keeps(pj[nb], pj[e], r[nb], r[e]) {
            return false;
        }
        movers.push((j, r));
    }
    let before: Vec<(usize, Vec<P3>, BTreeSet<(usize, u8)>, Hits)> = movers
        .iter()
        .map(|(x, _)| {
            let old = w.ls[*x].pts.clone();
            let t = w.touching(*x, &old);
            let h = w.hits(*x, &old);
            (*x, old, t, h)
        })
        .collect();
    for (x, r) in &movers {
        w.set(*x, r.clone());
    }
    let ok = before.iter().all(|(x, _, t, h)| {
        let cur = &w.ls[*x].pts;
        fits(cur, w.radius[*x]) && &w.touching(*x, cur) == t && within(&w.hits(*x, cur), h)
    });
    if ok {
        for (x, _) in movers.iter().skip(1) {
            w.bonds.push((i, *x));
        }
    } else {
        for (x, old, _, _) in before {
            w.set(x, old);
        }
    }
    ok
}

/// The changes of one sweep over the movable lines, in a fixed order.
fn sweep(w: &mut World, origin: (f64, f64)) -> Vec<usize> {
    let mut order: Vec<usize> = (0..w.ls.len()).filter(|&i| !w.ls[i].fixed() && w.ls[i].pts.len() >= 3).collect();
    let key = |l: &Line| {
        let f = l.pts[0];
        (l.tier, l.key.clone(), l.storey, ((f.0 - origin.0) * 1e3).round() as i64, ((f.1 - origin.1) * 1e3).round() as i64)
    };
    order.sort_by_key(|&i| (key(&w.ls[i]), i));
    let mut changed = vec![];
    for i in order {
        let mut any = false;
        let t = tidy(w.ls[i].pts.clone());
        if t.len() < w.ls[i].pts.len() && admissible(w, i, &t) {
            w.set(i, t);
            any = true;
        }
        for _ in 0..32 {
            match best_shortcut(w, i) {
                Some(q) => {
                    w.set(i, q);
                    any = true;
                }
                None => break,
            }
        }
        any |= snap(w, i, origin);
        if any {
            changed.push(i);
        }
    }
    changed
}

/// Faults the model reports, by line: fittings that do not fit, nodes, exact clashes; and
/// the counts of every services error, duct fault and clash.
fn audit(p: &GProject, ls: &[Line]) -> (BTreeMap<Src, usize>, [usize; 3]) {
    let mut by: BTreeMap<Src, usize> = BTreeMap::new();
    let ids: BTreeMap<usize, Src> = ls.iter().filter_map(|l| void_id(p, l.src).map(|v| (v, l.src))).collect();
    let mut hard = 0;
    for n in super::super::void::near_pairs(p, 0.0, &|_, _| 0.0) {
        if n.depth <= TOL {
            continue;
        }
        hard += 1;
        for r in [n.a.1, n.b.1].into_iter().flatten() {
            if let Some(s) = ids.get(&r) {
                *by.entry(*s).or_default() += 1;
            }
        }
    }
    let all: Vec<(Src, &super::super::GRun)> = p
        .services
        .water
        .iter()
        .enumerate()
        .map(|(i, r)| (Src::Water(i), r))
        .chain(p.services.elec.iter().enumerate().map(|(i, r)| (Src::Elec(i), r)))
        .collect();
    for sys in super::super::SvcSys::ALL {
        let mine: Vec<&(Src, &super::super::GRun)> = all.iter().filter(|x| x.1.sys == sys).collect();
        let runs: Vec<&super::super::GRun> = mine.iter().map(|x| x.1).collect();
        for (_, f) in svc::net_faults_at(&runs, &[]) {
            if let Some(k) = f.strip_prefix("run ").and_then(|r| r.split_once(' ')).and_then(|(n, _)| n.parse::<usize>().ok()) {
                *by.entry(mine[k].0).or_default() += 1;
            }
        }
    }
    let ducts = svc::duct_faults(p);
    for f in &ducts {
        if let Some(k) = f.split_once("duct ").and_then(|(_, r)| r.split_once(' ')).and_then(|(n, _)| n.parse::<usize>().ok()) {
            *by.entry(Src::Duct(k)).or_default() += 1;
        }
    }
    (by, [hard, svc::services_errors(p).len(), ducts.len()])
}

/// Lines bonded to `i` by the slides, `i` among them.
fn bonded(w: &World, i: usize) -> BTreeSet<usize> {
    let mut out = BTreeSet::from([i]);
    loop {
        let n = out.len();
        for &(a, b) in &w.bonds {
            if out.contains(&a) || out.contains(&b) {
                out.insert(a);
                out.insert(b);
            }
        }
        if out.len() == n {
            return out;
        }
    }
}

/// Hard clashes, soft clashes, tier faults, bends and length of the services as they stand.
fn gauge(p: &mut GProject, w: &World, origin: (f64, f64)) -> (u64, u64, u64, u64, f64) {
    for l in &w.ls {
        *points_mut(p, l.src) = l.pts.clone();
    }
    let m = super::metrics::route_metrics(p, origin);
    let n = |k: &str| m["total"][k].as_u64().unwrap_or(0);
    let sum = |k: &str| m[k].as_object().map_or(0, |o| o.values().filter_map(|v| v.as_u64().or_else(|| v["bends"].as_u64())).sum());
    (n("hard_clashes"), n("soft_clashes"), sum("tier_violations"), sum("systems"), m["total"]["length_m"].as_f64().unwrap_or(0.0))
}

pub(crate) fn run(p: &mut GProject, origin: (f64, f64)) {
    let mut w = World::new(p);
    if w.ls.is_empty() {
        return;
    }
    let orig: Vec<Vec<P3>> = w.ls.iter().map(|l| l.pts.clone()).collect();
    let (base_by, base_n) = audit(p, &w.ls);
    // each step stays only while the metrics it is judged by get no worse
    let mut g = gauge(p, &w, origin);
    for step in 0..5 {
        let keep: Vec<Vec<P3>> = w.ls.iter().map(|l| l.pts.clone()).collect();
        match step {
            1 => {
                super::lanes::tiers(&mut w, p);
            }
            2 => {
                super::lanes::lanes(&mut w, p, origin);
            }
            _ => {
                sweep(&mut w, origin);
            }
        }
        let h = gauge(p, &w, origin);
        let fair = h.0 <= g.0 && h.1 <= g.1 && h.2 <= g.2 && h.3 <= g.3 && h.4 <= g.4 + 1e-6;
        if fair && (step != 1 || h.2 < g.2) {
            g = h;
        } else {
            for (i, q) in keep.into_iter().enumerate() {
                if w.ls[i].pts != q {
                    w.set(i, q);
                }
            }
        }
    }
    for i in 0..w.ls.len() {
        *points_mut(p, w.ls[i].src) = w.ls[i].pts.clone();
    }
    let mut changed: BTreeSet<usize> = (0..w.ls.len()).filter(|&i| w.ls[i].pts != orig[i]).collect();
    for &i in &changed {
        *points_mut(p, w.ls[i].src) = w.ls[i].pts.clone();
    }
    // a line that brought a new fault goes back with the lines it moved with; an unexplained
    // fault takes the whole pass back
    for _ in 0..8 {
        let (by, n) = audit(p, &w.ls);
        let worse = |i: usize| by.get(&w.ls[i].src).copied().unwrap_or(0) > base_by.get(&w.ls[i].src).copied().unwrap_or(0);
        let bad: Vec<usize> = changed.iter().copied().filter(|&i| worse(i)).collect();
        if bad.is_empty() && (0..3).all(|k| n[k] <= base_n[k]) {
            return;
        }
        let back: BTreeSet<usize> =
            if bad.is_empty() { changed.iter().copied().collect() } else { bad.into_iter().flat_map(|i| bonded(&w, i)).collect() };
        for i in back {
            *points_mut(p, w.ls[i].src) = orig[i].clone();
            w.set(i, orig[i].clone());
            changed.remove(&i);
        }
        if changed.is_empty() {
            return;
        }
    }
    for &i in &changed {
        *points_mut(p, w.ls[i].src) = orig[i].clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_drops_straight_through_points() {
        let q = tidy(vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (2.0, 0.0, 0.0), (2.0, 1.0, 0.0), (2.0, 1.0, 0.0)]);
        assert_eq!(q, vec![(0.0, 0.0, 0.0), (2.0, 0.0, 0.0), (2.0, 1.0, 0.0)]);
    }

    #[test]
    fn a_z_jog_has_an_l_shortcut_and_an_aligned_detour_a_straight_one() {
        let z = shortcuts((0.0, 0.0, 0.0), (3.0, 0.2, 0.0));
        assert_eq!(z.len(), 2);
        assert!(z.iter().all(|c| bends(c) == 1));
        let u = shortcuts((0.0, 0.0, 0.0), (3.0, 0.0, 0.0));
        assert_eq!(u, vec![vec![(0.0, 0.0, 0.0), (3.0, 0.0, 0.0)]]);
    }

    #[test]
    fn a_straight_too_short_for_its_two_elbows_does_not_fit() {
        let q = [(0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (1.0, 0.1, 0.0), (2.0, 0.1, 0.0)];
        assert!(!fits(&q, 0.1));
        assert!(fits(&q, 0.04));
    }
}
