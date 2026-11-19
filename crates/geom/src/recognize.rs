//! Wall recognition from plan lines.
//!
//! Pairs of parallel opposing segments at a distance within the thickness range give axis pieces;
//! collinear pieces merge across gaps (openings or abutments), axis ends
//! are extended to their neighbours so that `join` builds L/T/X; crossings are cut by `resolve_crossings`.
//! Complexity is O(n log n) plus an offset window; the traversal order is fixed, so the result is deterministic.

use std::f64::consts::PI;

use crate::grid::{bbox_of, BoxGrid};
use crate::layered::{resolve_crossings, Layer, Strip};
use crate::math::{Tol, V2};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg {
    pub a: V2,
    pub b: V2,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub min_thickness: f64,
    pub max_thickness: f64,
    /// Length tolerance (offset coincidence, interval touching).
    pub tol: f64,
    /// Parallelism tolerance, rad.
    pub angle_tol: f64,
    /// Shorter segments are noise (hatching, ticks, opening reveals); also the minimum overlap of a pair.
    pub min_length: f64,
    /// Largest gap across which pieces of one wall are merged.
    pub max_gap: f64,
    pub height: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            min_thickness: 0.05,
            max_thickness: 0.6,
            tol: 1e-6,
            angle_tol: 1e-6,
            min_length: 0.5,
            max_gap: 2.5,
            height: 3.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum IssueKind {
    /// A gap in a wall with no abutting wall is a likely opening; `at` is the midpoint on the axis.
    Opening { width: f64 },
    /// Long segment without a pair.
    Unmatched { length: f64 },
    /// Short segment (hatching, etc.) that is neither an opening reveal nor a wall end face.
    Noise { length: f64 },
    /// Axis end with no neighbour.
    FreeEnd,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub kind: IssueKind,
    pub at: V2,
}

#[derive(Debug)]
pub struct Recognized {
    pub strips: Vec<Strip>,
    pub issues: Vec<Issue>,
}

fn dot(a: V2, b: V2) -> f64 {
    a.x * b.x + a.y * b.y
}

/// Wall axis in the (d, n) frame: point = n*c + d*t.
#[derive(Clone, Debug)]
struct Line {
    d: V2,
    n: V2,
    c: f64,
    w: f64,
    t0: f64,
    t1: f64,
    gaps: Vec<(f64, f64)>,
}

impl Line {
    fn pt(&self, t: f64) -> V2 {
        self.n * self.c + self.d * t
    }
}

struct Piece {
    cluster: usize,
    c: f64,
    w: f64,
    t0: f64,
    t1: f64,
}

pub fn recognize_walls(segs: &[Seg], opt: &Options) -> Recognized {
    let tol = opt.tol;
    let mut short = vec![];
    let mut keyed = vec![];
    for (i, s) in segs.iter().enumerate() {
        let v = s.b - s.a;
        if v.len() < opt.min_length {
            short.push(i);
            continue;
        }
        let mut th = v.y.atan2(v.x);
        if th < 0.0 {
            th += PI;
        }
        if th >= PI - opt.angle_tol {
            th -= PI;
        }
        keyed.push((th, i));
    }
    keyed.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));

    let mut clusters: Vec<Vec<usize>> = vec![];
    let mut prev = f64::NEG_INFINITY;
    for &(th, i) in &keyed {
        if th - prev > opt.angle_tol || clusters.is_empty() {
            clusters.push(vec![]);
        }
        clusters.last_mut().unwrap().push(i);
        prev = th;
    }

    let mut used = vec![false; segs.len()];
    let mut pieces = vec![];
    let mut frames = vec![];
    for (ci, cl) in clusters.iter().enumerate() {
        let unit = |i: usize| {
            let v = segs[i].b - segs[i].a;
            v * (1.0 / v.len())
        };
        let u0 = unit(cl[0]);
        let mut sum = V2::default();
        for &i in cl {
            let u = unit(i);
            sum = sum + if dot(u, u0) < 0.0 { -u } else { u };
        }
        let d = sum * (1.0 / sum.len());
        let n = V2 { x: -d.y, y: d.x };
        frames.push((d, n));
        // (offset, t0, t1, index)
        let mut items: Vec<(f64, f64, f64, usize)> = cl
            .iter()
            .map(|&i| {
                let s = segs[i];
                let (ta, tb) = (dot(d, s.a), dot(d, s.b));
                (dot(n, (s.a + s.b) * 0.5), ta.min(tb), ta.max(tb), i)
            })
            .collect();
        items.sort_by(|x, y| {
            x.0.total_cmp(&y.0)
                .then(x.1.total_cmp(&y.1))
                .then(x.3.cmp(&y.3))
        });
        // Runs of equal offset (by t0) and the longest length in a run: within a run,
        // overlapping candidates are found by binary search rather than scanning the whole line.
        let mut runs: Vec<(usize, usize, f64)> = vec![];
        for (k, it) in items.iter().enumerate() {
            match runs.last_mut() {
                Some(r) if it.0 == items[r.0].0 => {
                    r.1 = k + 1;
                    r.2 = r.2.max(it.2 - it.1);
                }
                _ => runs.push((k, k + 1, it.2 - it.1)),
            }
        }
        for k in 0..items.len() {
            let (o, t0, t1, i) = items[k];
            let lo = items.partition_point(|it| it.0 < o + opt.min_thickness - tol);
            let overlap = |it: &(f64, f64, f64, usize)| it.2.min(t1) - it.1.max(t0);
            let mut best: Option<f64> = None;
            let mut r = runs.partition_point(|r| r.1 <= lo);
            while r < runs.len() {
                let (rs, re, rmax) = runs[r];
                r += 1;
                let off = items[rs].0;
                if off > o + opt.max_thickness + tol || best.is_some_and(|b| off > b + tol) {
                    break;
                }
                let rs = rs.max(lo);
                let run = &items[rs..re];
                // Overlap ≥ min_length ⇒ candidate t0 in (t0 − rmax, t1 − min_length].
                let a = run.partition_point(|it| it.1 <= t0 - rmax - tol);
                let b = run.partition_point(|it| it.1 <= t1 - opt.min_length + tol);
                for it in &run[a..b] {
                    if it.3 == i || overlap(it) < opt.min_length {
                        continue;
                    }
                    best.get_or_insert(it.0);
                    used[i] = true;
                    used[it.3] = true;
                    pieces.push(Piece {
                        cluster: ci,
                        c: (o + it.0) * 0.5,
                        w: it.0 - o,
                        t0: it.1.max(t0),
                        t1: it.2.min(t1),
                    });
                }
            }
        }
    }

    // Merge collinear pieces of equal thickness.
    pieces.sort_by(|x, y| {
        x.cluster
            .cmp(&y.cluster)
            .then(x.c.total_cmp(&y.c))
            .then(x.w.total_cmp(&y.w))
            .then(x.t0.total_cmp(&y.t0))
    });
    let mut lines: Vec<Line> = vec![];
    let mut k = 0;
    while k < pieces.len() {
        let p0 = &pieces[k];
        let mut e = k + 1;
        while e < pieces.len()
            && pieces[e].cluster == p0.cluster
            && (pieces[e].c - p0.c).abs() <= tol
            && (pieces[e].w - p0.w).abs() <= tol
        {
            e += 1;
        }
        let mut grp: Vec<&Piece> = pieces[k..e].iter().collect();
        grp.sort_by(|x, y| x.t0.total_cmp(&y.t0).then(x.t1.total_cmp(&y.t1)));
        let (d, n) = frames[p0.cluster];
        let mut cur: Option<Line> = None;
        for p in grp {
            match cur.as_mut() {
                Some(l) if p.t0 <= l.t1 + tol => l.t1 = l.t1.max(p.t1),
                Some(l) if p.t0 - l.t1 <= opt.max_gap => {
                    l.gaps.push((l.t1, p.t0));
                    l.t1 = p.t1;
                }
                _ => {
                    lines.extend(cur.take());
                    cur = Some(Line {
                        d,
                        n,
                        c: p0.c,
                        w: p0.w,
                        t0: p.t0,
                        t1: p.t1,
                        gaps: vec![],
                    });
                }
            }
        }
        lines.extend(cur);
        k = e;
    }

    // Extend ends to neighbour axes (using the original axes, then applied all at once).
    let mut issues = vec![];
    let mut jambs = vec![];
    let mut ends = vec![];
    // Neighbours via a grid: axis boxes padded by the maximum reach.
    let wmax = lines.iter().fold(0.0f64, |m, l| m.max(l.w));
    let pad = wmax + 2.0 * tol;
    let line_grid = |lines: &[Line]| {
        BoxGrid::new(
            &lines
                .iter()
                .map(|l| bbox_of(&[l.pt(l.t0), l.pt(l.t1)], pad))
                .collect::<Vec<_>>(),
        )
    };
    let grid = line_grid(&lines);
    let mut cand = vec![];
    for (li, l) in lines.iter().enumerate() {
        let mut new = [l.t0, l.t1];
        for (side, &te) in [l.t0, l.t1].iter().enumerate() {
            let mut best: Option<(f64, f64)> = None;
            grid.query(&bbox_of(&[l.pt(te)], pad), &mut cand);
            for &mi in &cand {
                let m = &lines[mi];
                let den = l.d.cross(m.d);
                if mi == li || den.abs() <= opt.angle_tol.max(1e-9) {
                    continue;
                }
                let (pl, pm) = (l.n * l.c, m.n * m.c);
                let s = (pm - pl).cross(m.d) / den;
                let u = (pl - pm).cross(l.d) / m.d.cross(l.d);
                let reach = (l.w + m.w) * 0.5 + tol;
                let dist = (s - te).abs();
                if dist <= reach
                    && u >= m.t0 - reach
                    && u <= m.t1 + reach
                    && best.is_none_or(|b| dist < b.0)
                {
                    best = Some((dist, s));
                }
            }
            match best {
                Some((_, s)) => new[side] = s,
                None => {
                    let p = l.pt(te);
                    jambs.push((p + l.n * (l.w * 0.5), p - l.n * (l.w * 0.5)));
                    issues.push(Issue {
                        kind: IssueKind::FreeEnd,
                        at: p,
                    })
                }
            }
        }
        ends.push(new);
    }
    for (l, [t0, t1]) in lines.iter_mut().zip(ends) {
        (l.t0, l.t1) = (t0.min(t1), t0.max(t1));
    }

    // Gaps: an abutting foreign wall or an opening.
    let mut openings = vec![];
    let grid = line_grid(&lines);
    for (li, l) in lines.iter().enumerate() {
        for &(g0, g1) in &l.gaps {
            let mid = l.pt((g0 + g1) * 0.5);
            grid.query(&bbox_of(&[mid], pad), &mut cand);
            let junction = cand.iter().any(|&mi| {
                let m = &lines[mi];
                mi != li
                    && l.d.cross(m.d).abs() > opt.angle_tol.max(1e-9)
                    && (dot(m.n, mid) - m.c).abs() <= m.w * 0.5 + tol
                    && dot(m.d, mid) >= m.t0 - tol
                    && dot(m.d, mid) <= m.t1 + tol
            });
            if !junction {
                openings.push(Issue {
                    kind: IssueKind::Opening { width: g1 - g0 },
                    at: mid,
                });
                for g in [g0, g1] {
                    let p = l.pt(g);
                    jambs.push((p + l.n * (l.w * 0.5), p - l.n * (l.w * 0.5)));
                }
            }
        }
    }
    let near = |p: V2, q: V2| (p - q).len() <= tol.max(1e-9) * 10.0;
    let mut out = openings;
    out.extend(issues);
    for (i, s) in segs.iter().enumerate() {
        if !used[i] && (s.b - s.a).len() >= opt.min_length {
            out.push(Issue {
                kind: IssueKind::Unmatched {
                    length: (s.b - s.a).len(),
                },
                at: (s.a + s.b) * 0.5,
            });
        }
    }
    // Reveals by cells: ends closer than `near` lie in neighbouring cells of pitch 2·near.
    let h = tol.max(1e-9) * 20.0;
    let cell = |p: V2| ((p.x / h).floor() as i64, (p.y / h).floor() as i64);
    let mut by_cell: std::collections::HashMap<(i64, i64), Vec<usize>> = Default::default();
    for (k, &(p, q)) in jambs.iter().enumerate() {
        by_cell.entry(cell(p)).or_default().push(k);
        by_cell.entry(cell(q)).or_default().push(k);
    }
    for &i in &short {
        let s = segs[i];
        let (cx, cy) = cell(s.a);
        let jamb = (-1..=1).any(|dx| {
            (-1..=1).any(|dy| {
                by_cell.get(&(cx + dx, cy + dy)).is_some_and(|ks| {
                    ks.iter().any(|&k| {
                        let (p, q) = jambs[k];
                        (near(s.a, p) && near(s.b, q)) || (near(s.a, q) && near(s.b, p))
                    })
                })
            })
        });
        if !jamb {
            out.push(Issue {
                kind: IssueKind::Noise {
                    length: (s.b - s.a).len(),
                },
                at: (s.a + s.b) * 0.5,
            });
        }
    }

    let strips: Vec<Strip> = lines
        .iter()
        .filter(|l| l.t1 - l.t0 > tol)
        .map(|l| Strip {
            a: l.pt(l.t0),
            b: l.pt(l.t1),
            layers: vec![Layer::new(l.w)],
            offset0: -l.w * 0.5,
            base: 0.0,
            height: opt.height,
        })
        .collect();
    let t = Tol {
        length: tol,
        ..Tol::default()
    };
    Recognized {
        strips: resolve_crossings(&strips, t),
        issues: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layered::{join, layer_footprints};
    use crate::math::v2;

    #[derive(Clone, Copy)]
    struct R(f64, f64, f64, f64);

    fn inside(r: &R, x: f64, y: f64) -> bool {
        x > r.0 && x < r.1 && y > r.2 && y < r.3
    }

    fn coords(v: impl Iterator<Item = f64>) -> Vec<f64> {
        let mut c: Vec<f64> = v.collect();
        c.sort_by(f64::total_cmp);
        c.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        c
    }

    /// Outline of the union of wall rectangles minus openings (as plan lines) and the wall area without openings.
    fn plan_lines(walls: &[R], doors: &[R]) -> (Vec<Seg>, f64) {
        let all: Vec<R> = walls.iter().chain(doors).copied().collect();
        let xs = coords(all.iter().flat_map(|r| [r.0, r.1]));
        let ys = coords(all.iter().flat_map(|r| [r.2, r.3]));
        let (nx, ny) = (xs.len() - 1, ys.len() - 1);
        let wall = |i: usize, j: usize| {
            let (x, y) = ((xs[i] + xs[i + 1]) / 2.0, (ys[j] + ys[j + 1]) / 2.0);
            walls.iter().any(|r| inside(r, x, y))
        };
        let cov = |i: usize, j: usize| {
            let (x, y) = ((xs[i] + xs[i + 1]) / 2.0, (ys[j] + ys[j + 1]) / 2.0);
            wall(i, j) && !doors.iter().any(|r| inside(r, x, y))
        };
        let mut area = 0.0;
        for i in 0..nx {
            for j in 0..ny {
                if wall(i, j) {
                    area += (xs[i + 1] - xs[i]) * (ys[j + 1] - ys[j]);
                }
            }
        }
        let mut segs = vec![];
        // Vertical edges.
        for i in 0..=nx {
            let mut run: Option<(usize, bool)> = None;
            for j in 0..=ny {
                let st = (j < ny).then(|| {
                    let l = i > 0 && cov(i - 1, j);
                    let r = i < nx && cov(i, j);
                    (l != r).then_some(r)
                });
                let st = st.flatten();
                if let Some((j0, side)) = run {
                    if st != Some(side) {
                        segs.push(Seg {
                            a: v2(xs[i], ys[j0]),
                            b: v2(xs[i], ys[j]),
                        });
                        run = None;
                    }
                }
                if run.is_none() {
                    run = st.map(|s| (j, s));
                }
            }
        }
        for j in 0..=ny {
            let mut run: Option<(usize, bool)> = None;
            for i in 0..=nx {
                let st = (i < nx).then(|| {
                    let b = j > 0 && cov(i, j - 1);
                    let t = j < ny && cov(i, j);
                    (b != t).then_some(t)
                });
                let st = st.flatten();
                if let Some((i0, side)) = run {
                    if st != Some(side) {
                        segs.push(Seg {
                            a: v2(xs[i0], ys[j]),
                            b: v2(xs[i], ys[j]),
                        });
                        run = None;
                    }
                }
                if run.is_none() {
                    run = st.map(|s| (i, s));
                }
            }
        }
        (segs, area)
    }

    fn joined_area(strips: &[Strip]) -> f64 {
        let tol = Tol::default();
        let j = join(strips, tol).expect("join");
        let mut sum = 0.0;
        for (i, s) in strips.iter().enumerate() {
            for f in layer_footprints(s, &j.pieces[i], i, tol)
                .unwrap()
                .iter()
                .flatten()
            {
                sum += f.area();
            }
        }
        sum + j
            .fillers
            .iter()
            .map(|f| crate::profile::signed_area(f).abs())
            .sum::<f64>()
    }

    fn hx(y: f64, x0: f64, x1: f64, w: f64) -> R {
        R(x0, x1, y - w / 2.0, y + w / 2.0)
    }
    fn vy(x: f64, y0: f64, y1: f64, w: f64) -> R {
        R(x - w / 2.0, x + w / 2.0, y0, y1)
    }

    #[test]
    fn building_12x8_with_partitions_doors_and_noise() {
        let (e, p) = (0.3, 0.12);
        let walls = [
            hx(0.0, -0.15, 12.15, e),
            hx(8.0, -0.15, 12.15, e),
            vy(0.0, -0.15, 8.15, e),
            vy(12.0, -0.15, 8.15, e),
            vy(6.0, 0.0, 8.0, p),
            hx(4.0, 0.0, 12.0, p),
        ];
        let doors = [
            hx(0.0, 2.0, 2.9, e),
            hx(4.0, 8.0, 8.8, p),
            vy(6.0, 1.5, 2.3, p),
        ];
        let (mut segs, area) = plan_lines(&walls, &doors);
        let n_hatch = 8;
        for k in 0..n_hatch {
            let x = 8.0 + 0.25 * k as f64;
            segs.push(Seg {
                a: v2(x, -0.15),
                b: v2(x + 0.3, 0.15),
            });
        }
        segs.push(Seg {
            a: v2(3.0, 5.0),
            b: v2(3.2, 5.1),
        });
        segs.push(Seg {
            a: v2(0.0, -2.0),
            b: v2(12.0, -2.0),
        });

        let r = recognize_walls(&segs, &Options::default());
        let thick = |w: f64| {
            r.strips
                .iter()
                .filter(move |s| (s.layers[0].thickness - w).abs() < 1e-9)
        };
        assert_eq!(thick(e).count(), 4);
        let plen: f64 = thick(p).map(|s| s.length()).sum();
        assert!((plen - 20.0).abs() < 1e-9, "{plen}");
        assert_eq!(r.strips.len(), 7); // H is split into X by V

        let mut ops: Vec<(f64, f64, f64)> = r
            .issues
            .iter()
            .filter_map(|i| match i.kind {
                IssueKind::Opening { width } => Some((i.at.x, i.at.y, width)),
                _ => None,
            })
            .collect();
        ops.sort_by(|a, b| a.0.total_cmp(&b.0));
        let want = [(2.45, 0.0, 0.9), (6.0, 1.9, 0.8), (8.4, 4.0, 0.8)];
        assert_eq!(ops.len(), 3, "{:?}", r.issues);
        for (g, w) in ops.iter().zip(want) {
            assert!(
                (g.0 - w.0).abs() < 1e-9 && (g.1 - w.1).abs() < 1e-9 && (g.2 - w.2).abs() < 1e-9
            );
        }
        let count = |f: fn(&IssueKind) -> bool| r.issues.iter().filter(|i| f(&i.kind)).count();
        assert_eq!(count(|k| matches!(k, IssueKind::Noise { .. })), n_hatch + 1);
        assert_eq!(count(|k| matches!(k, IssueKind::Unmatched { .. })), 1);
        assert_eq!(count(|k| matches!(k, IssueKind::FreeEnd)), 0);

        let got = joined_area(&r.strips);
        assert!((got - area).abs() < 1e-9, "{got} vs {area}");
    }

    #[test]
    fn deterministic_under_input_order() {
        let walls = [hx(0.0, -0.1, 5.1, 0.2), vy(0.0, -0.1, 4.0, 0.2)];
        let (segs, _) = plan_lines(&walls, &[]);
        let a = recognize_walls(&segs, &Options::default());
        let b = recognize_walls(&segs, &Options::default());
        assert_eq!(format!("{:?}", a), format!("{:?}", b));
        assert_eq!(a.strips.len(), 2);
        assert_eq!(a.issues.len(), 2); // two free ends
    }

    #[test]
    fn grid_of_40_walls() {
        let (e, p) = (0.3, 0.12);
        let gx = [0.0, 4.0, 8.0, 12.0, 16.0];
        let gy = [0.0, 3.0, 6.0, 9.0, 12.0];
        let mut walls = vec![
            hx(0.0, -0.15, 16.15, e),
            hx(12.0, -0.15, 16.15, e),
            vy(0.0, -0.15, 12.15, e),
            vy(16.0, -0.15, 12.15, e),
        ];
        for &x in &gx[1..4] {
            walls.push(vy(x, 0.0, 12.0, p));
        }
        for &y in &gy[1..4] {
            walls.push(hx(y, 0.0, 16.0, p));
        }
        // Reference: walls between grid nodes (a, b, thickness).
        let mut want = vec![];
        let mut doors = vec![];
        for (k, &y) in gy.iter().enumerate() {
            let w = if k == 0 || k == 4 { e } else { p };
            for i in 0..4 {
                want.push((v2(gx[i], y), v2(gx[i + 1], y), w));
                if (i + k) % 2 == 0 {
                    doors.push(hx(y, gx[i] + 1.0, gx[i] + 1.9, w));
                }
            }
        }
        for (k, &x) in gx.iter().enumerate() {
            let w = if k == 0 || k == 4 { e } else { p };
            for j in 0..4 {
                want.push((v2(x, gy[j]), v2(x, gy[j + 1]), w));
                if (j + k) % 2 == 1 {
                    doors.push(vy(x, gy[j] + 1.0, gy[j] + 1.8, w));
                }
            }
        }
        assert_eq!(want.len(), 40);
        let (segs, area) = plan_lines(&walls, &doors);
        let t = web_time::Instant::now();
        let r = recognize_walls(&segs, &Options::default());
        let dt = t.elapsed();

        let on = |q: V2, w: f64| {
            r.strips.iter().any(|s| {
                let rel = q - s.a;
                let along = dot(rel, s.dir());
                (s.layers[0].thickness - w).abs() < 1e-9
                    && dot(rel, s.normal()).abs() < 1e-6
                    && along >= -1e-9
                    && along <= s.length() + 1e-9
            })
        };
        let ok = want
            .iter()
            .filter(|&&(a, b, w)| [0.1, 0.5, 0.9].iter().all(|&f| on(a + (b - a) * f, w)))
            .count();
        let ops = r
            .issues
            .iter()
            .filter(|i| matches!(i.kind, IssueKind::Opening { .. }))
            .count();
        println!(
            "grid: {ok}/40 walls, {} strips, {ops}/{} openings, {} segs, {:?}",
            r.strips.len(),
            doors.len(),
            segs.len(),
            dt
        );
        assert!(ok * 100 >= 95 * 40);
        assert_eq!(ops, doors.len());
        let got = joined_area(&r.strips);
        assert!((got - area).abs() < 1e-9, "{got} vs {area}");
    }

    /// Large plan: an n×n grid of 4 m rooms, a door in every wall, hatching in each room, plus
    /// long standalone lines (axes, frames). ~2·n² walls.
    pub(crate) fn big_plan(n: usize) -> Vec<Seg> {
        let (t, g) = (0.2, 0.9);
        let mut v = vec![];
        let mut push = |a: V2, b: V2| v.push(Seg { a, b });
        for j in 0..=n {
            for i in 0..n {
                let (x, y) = (i as f64 * 4.0, j as f64 * 4.0);
                for s in [-t / 2.0, t / 2.0] {
                    push(v2(x, y + s), v2(x + 1.5, y + s));
                    push(v2(x + 1.5 + g, y + s), v2(x + 4.0, y + s));
                    push(v2(y + s, x), v2(y + s, x + 1.5));
                    push(v2(y + s, x + 1.5 + g), v2(y + s, x + 4.0));
                }
                for k in 0..8 {
                    let (hx, hy) = (x + 0.6 + k as f64 * 0.35, y + 1.0);
                    push(v2(hx, hy), v2(hx + 0.2, hy + 0.2));
                }
            }
        }
        for k in 0..=n {
            let c = k as f64 * 4.0 + 2.0;
            push(v2(-1.0, c), v2(n as f64 * 4.0 + 1.0, c));
        }
        v
    }

    #[test]
    #[ignore = "timing; run with --ignored --nocapture"]
    fn recognize_scales_near_linearly() {
        let opt = Options::default();
        let mut prev: Option<(usize, f64)> = None;
        for n in [40, 80, 160] {
            let segs = big_plan(n);
            let t = web_time::Instant::now();
            let r = recognize_walls(&segs, &opt);
            let dt = t.elapsed().as_secs_f64();
            eprintln!(
                "n={n} segs={} strips={} issues={} {dt:.3}s",
                segs.len(),
                r.strips.len(),
                r.issues.len()
            );
            if let Some((m, p)) = prev {
                let k = segs.len() as f64 / m as f64;
                assert!(dt < p * k * 1.6 + 0.05, "superlinear: {p:.3}s -> {dt:.3}s");
            }
            prev = Some((segs.len(), dt));
        }
    }
}
