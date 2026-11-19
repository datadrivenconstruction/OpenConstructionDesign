// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 2D sketch solver: points, lines (point pairs), constraints. Levenberg–Marquardt with a
//! minimum-norm step (an underdetermined system moves as little as possible), followed by a
//! degrees-of-freedom analysis of the Jacobian at the solution: rank, free points (outside the row
//! space), redundant constraints (rows linearly dependent on earlier ones), and conflicting ones,
//! reported as a minimal set where dropping any member makes the rest satisfiable (culprit first).
//! Pins (`Fix*`) eliminate variables rather than add equations; dragged points
//! (`pinned`) are pinned after them, so `Fix` always wins over dragging.
//! Geometry, dense Gaussian elimination, row basis and minimal-conflict search are shared with `ddc_solver`.

use ddc_solver::{dir, gauss, min_conflict_set, pt, sdist, unit, wrap_angle as wrap, RowSpace};
pub use ddc_solver::{L, P};

#[derive(Clone, Debug, PartialEq)]
pub enum Geo {
    Fix(P, f64, f64),
    FixX(P, f64),
    FixY(P, f64),
    Coincident(P, P),
    Horizontal(L),
    Vertical(L),
    /// |pq| = d (d = 0 means coincident points, two equations).
    Dist(P, P, f64),
    /// Signed distance from the line to the point (positive to the left of the direction) = d.
    PointLine(P, L, f64),
    /// Both points of the second line at signed distance d from the first (parallel offset).
    LineDist(L, L, f64),
    Parallel(L, L),
    Perp(L, L),
    /// Counter-clockwise angle from the first line's direction to the second's, rad.
    Angle(L, L, f64),
    EqualLen(L, L),
    /// Chain of lines: adjacent signed distances are equal. Parallelism is not included and is
    /// specified separately (for reference planes it follows from the pinned directions).
    EqualGaps(Vec<L>),
    /// The point is the midpoint of the segment.
    Midpoint(P, L),
    /// Signed horizontal distance: x(b) - x(a) = d.
    DistX(P, P, f64),
    /// Signed vertical distance: y(b) - y(a) = d.
    DistY(P, P, f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Con {
    /// Caller-supplied key, used in the report.
    pub tag: i64,
    pub geo: Geo,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sketch {
    pub pts: Vec<[f64; 2]>,
    pub cons: Vec<Con>,
}

impl Sketch {
    pub fn point(&mut self, x: f64, y: f64) -> P {
        self.pts.push([x, y]);
        self.pts.len() - 1
    }
    pub fn add(&mut self, tag: i64, geo: Geo) {
        self.cons.push(Con { tag, geo });
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub max_iter: usize,
    /// Residual tolerance (m, rad).
    pub tol: f64,
    /// Points that must not move (just dragged); `Fix*` takes precedence.
    pub pinned: Vec<P>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_iter: 200,
            tol: 1e-9,
            pinned: vec![],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Fully determined, with no redundant constraints.
    Well,
    /// Degrees of freedom remain.
    Under,
    /// There are redundant (consistent) constraints.
    Over,
    /// The constraints conflict.
    Conflict,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub pts: Vec<[f64; 2]>,
    pub converged: bool,
    pub iterations: usize,
    /// Largest residual at the solution.
    pub residual: f64,
    pub dof: usize,
    /// Points with at least one coordinate not determined by the constraints.
    pub free_points: Vec<P>,
    /// Tags of constraints whose equations depend on earlier ones.
    pub redundant: Vec<i64>,
    /// Tags of the minimal conflicting set (culprit first) and their residuals at the
    /// least-squares solution.
    pub conflicting: Vec<(i64, f64)>,
}

impl Report {
    pub fn verdict(&self) -> Verdict {
        if !self.conflicting.is_empty() {
            Verdict::Conflict
        } else if !self.redundant.is_empty() {
            Verdict::Over
        } else if self.dof > 0 {
            Verdict::Under
        } else {
            Verdict::Well
        }
    }
}

const CHECK: f64 = 1e-6;
/// Distance below which `Dist` means coincident points (|q−p| has no derivative at zero).
const ZERO_DIST: f64 = 1e-12;

fn residuals(g: &Geo, x: &[f64], out: &mut Vec<f64>) {
    match g {
        Geo::Fix(..) | Geo::FixX(..) | Geo::FixY(..) => {}
        Geo::Coincident(a, b) => {
            let (p, q) = (pt(x, *a), pt(x, *b));
            out.push(q.0 - p.0);
            out.push(q.1 - p.1);
        }
        Geo::Dist(a, b, d) if d.abs() < ZERO_DIST => {
            let (p, q) = (pt(x, *a), pt(x, *b));
            out.push(q.0 - p.0);
            out.push(q.1 - p.1);
        }
        Geo::Horizontal(l) => out.push(dir(x, *l).1),
        Geo::Vertical(l) => out.push(dir(x, *l).0),
        Geo::Dist(a, b, d) => {
            let (p, q) = (pt(x, *a), pt(x, *b));
            out.push((q.0 - p.0).hypot(q.1 - p.1) - d);
        }
        Geo::PointLine(p, l, d) => out.push(sdist(x, *l, *p) - d),
        Geo::LineDist(l1, l2, d) => {
            out.push(sdist(x, *l1, l2.0) - d);
            out.push(sdist(x, *l1, l2.1) - d);
        }
        Geo::Parallel(l1, l2) => {
            let (u, v) = (unit(x, *l1), unit(x, *l2));
            out.push(u.0 * v.1 - u.1 * v.0);
        }
        Geo::Perp(l1, l2) => {
            let (u, v) = (unit(x, *l1), unit(x, *l2));
            out.push(u.0 * v.0 + u.1 * v.1);
        }
        Geo::Angle(l1, l2, th) => {
            let (u, v) = (unit(x, *l1), unit(x, *l2));
            let a = (u.0 * v.1 - u.1 * v.0).atan2(u.0 * v.0 + u.1 * v.1);
            out.push(wrap(a - th));
        }
        Geo::EqualLen(l1, l2) => {
            let (a, b) = (dir(x, *l1), dir(x, *l2));
            out.push(a.0.hypot(a.1) - b.0.hypot(b.1));
        }
        Geo::EqualGaps(ls) => {
            if ls.len() < 2 {
                return;
            }
            let gap = |i: usize| sdist(x, ls[i], ls[i + 1].0);
            for i in 1..ls.len() - 1 {
                out.push(gap(i) - gap(0));
            }
        }
        Geo::Midpoint(p, l) => {
            let (m, a, b) = (pt(x, *p), pt(x, l.0), pt(x, l.1));
            out.push(m.0 - (a.0 + b.0) / 2.0);
            out.push(m.1 - (a.1 + b.1) / 2.0);
        }
        Geo::DistX(a, b, d) => out.push(pt(x, *b).0 - pt(x, *a).0 - d),
        Geo::DistY(a, b, d) => out.push(pt(x, *b).1 - pt(x, *a).1 - d),
    }
}

fn points_of(g: &Geo) -> Vec<P> {
    match g {
        Geo::Fix(p, ..) | Geo::FixX(p, _) | Geo::FixY(p, _) => vec![*p],
        Geo::Coincident(a, b)
        | Geo::Dist(a, b, _)
        | Geo::DistX(a, b, _)
        | Geo::DistY(a, b, _) => vec![*a, *b],
        Geo::Horizontal(l) | Geo::Vertical(l) => vec![l.0, l.1],
        Geo::PointLine(p, l, _) | Geo::Midpoint(p, l) => vec![*p, l.0, l.1],
        Geo::LineDist(a, b, _)
        | Geo::Parallel(a, b)
        | Geo::Perp(a, b)
        | Geo::Angle(a, b, _)
        | Geo::EqualLen(a, b) => vec![a.0, a.1, b.0, b.1],
        Geo::EqualGaps(ls) => ls.iter().flat_map(|l| [l.0, l.1]).collect(),
    }
}

/// System over the free variables: one row per constraint.
#[derive(Clone)]
struct Sys<'a> {
    cons: Vec<&'a Con>,
    /// Free variables of the constraint: (column, variable index).
    cols: Vec<Vec<(usize, usize)>>,
    n: usize,
}

impl<'a> Sys<'a> {
    fn rows(&self, x: &[f64]) -> Vec<Vec<f64>> {
        self.cons
            .iter()
            .map(|c| {
                let mut r = vec![];
                residuals(&c.geo, x, &mut r);
                r
            })
            .collect()
    }

    /// Jacobian by constraint: for each row, a dense row of length n.
    fn jac(&self, x: &mut [f64]) -> Vec<Vec<Vec<f64>>> {
        let mut out = Vec::with_capacity(self.cons.len());
        for (k, c) in self.cons.iter().enumerate() {
            let mut base = vec![];
            residuals(&c.geo, x, &mut base);
            let mut rows = vec![vec![0.0; self.n]; base.len()];
            for &(col, i) in &self.cols[k] {
                let old = x[i];
                let h = 1e-7 * (1.0 + old.abs());
                x[i] = old + h;
                let mut rp = vec![];
                residuals(&c.geo, x, &mut rp);
                x[i] = old - h;
                let mut rm = vec![];
                residuals(&c.geo, x, &mut rm);
                x[i] = old;
                for (j, row) in rows.iter_mut().enumerate() {
                    // angle: a difference across ±π must not produce a 2π jump
                    let d = match c.geo {
                        Geo::Angle(..) => wrap(rp[j] - rm[j]),
                        _ => rp[j] - rm[j],
                    };
                    row[col] = d / (2.0 * h);
                }
            }
            out.push(rows);
        }
        out
    }

    /// Subsystem of the constraints with indices `idx` (same variables).
    fn subset(&self, idx: &[usize]) -> Sys<'a> {
        Sys {
            cons: idx.iter().map(|&k| self.cons[k]).collect(),
            cols: idx.iter().map(|&k| self.cols[k].clone()).collect(),
            n: self.n,
        }
    }
}

fn sq(r: &[Vec<f64>]) -> f64 {
    r.iter().flatten().map(|v| v * v).sum()
}

fn worst(r: &[Vec<f64>]) -> f64 {
    r.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()))
}

/// LM step: (JᵀJ + λI)dx = −Jᵀr, via the dual form when m ≤ n (minimum norm).
fn lm_step(j: &[Vec<f64>], r: &[f64], n: usize, lambda: f64) -> Vec<f64> {
    let m = j.len();
    if m <= n {
        let mut a = vec![0.0; m * m];
        for p in 0..m {
            for q in p..m {
                let s: f64 = (0..n).map(|c| j[p][c] * j[q][c]).sum();
                a[p * m + q] = s;
                a[q * m + p] = s;
            }
            a[p * m + p] += lambda;
        }
        let y = gauss(a, r.iter().map(|v| -v).collect(), m);
        (0..n)
            .map(|c| (0..m).map(|k| j[k][c] * y[k]).sum())
            .collect()
    } else {
        let mut a = vec![0.0; n * n];
        for p in 0..n {
            for q in p..n {
                let s: f64 = (0..m).map(|k| j[k][p] * j[k][q]).sum();
                a[p * n + q] = s;
                a[q * n + p] = s;
            }
            a[p * n + p] += lambda;
        }
        let g: Vec<f64> = (0..n)
            .map(|c| -(0..m).map(|k| j[k][c] * r[k]).sum::<f64>())
            .collect();
        gauss(a, g, n)
    }
}

struct Run {
    x: Vec<f64>,
    rows: Vec<Vec<f64>>,
    iterations: usize,
    converged: bool,
}

/// Levenberg–Marquardt from `x0` over the free variables `free`.
fn lm(sys: &Sys, x0: &[f64], free: &[usize], opt: &Options) -> Run {
    let mut x = x0.to_vec();
    let (mut lambda, mut iterations, mut converged) = (1e-6, 0, false);
    let mut rows = sys.rows(&x);
    let mut cost = sq(&rows);
    while iterations < opt.max_iter {
        if rows.iter().flatten().all(|v| v.abs() < opt.tol) {
            converged = true;
            break;
        }
        if sys.n == 0 {
            break;
        }
        iterations += 1;
        let jb = sys.jac(&mut x);
        let j: Vec<Vec<f64>> = jb.into_iter().flatten().collect();
        let r: Vec<f64> = rows.iter().flatten().copied().collect();
        let grad = (0..sys.n)
            .map(|c| (0..j.len()).map(|k| j[k][c] * r[k]).sum::<f64>().abs())
            .fold(0.0, f64::max);
        if grad < 1e-15 {
            break;
        }
        let mut accepted = false;
        while lambda < 1e10 {
            let dx = lm_step(&j, &r, sys.n, lambda);
            let mut xt = x.clone();
            for (c, &i) in free.iter().enumerate() {
                xt[i] += dx[c];
            }
            let rt = sys.rows(&xt);
            let ct = sq(&rt);
            if ct.is_finite() && ct < cost {
                x = xt;
                rows = rt;
                cost = ct;
                lambda = (lambda / 4.0).max(1e-12);
                accepted = true;
                break;
            }
            lambda *= 8.0;
        }
        if !accepted {
            break;
        }
    }
    if rows.iter().flatten().all(|v| v.abs() < opt.tol) {
        converged = true;
    }
    Run {
        x,
        rows,
        iterations,
        converged,
    }
}

/// Solution and diagnostics. Never panics; non-finite inputs yield Conflict.
pub fn solve(sk: &Sketch, opt: &Options) -> Report {
    let nv = sk.pts.len() * 2;
    let mut x: Vec<f64> = sk.pts.iter().flat_map(|p| [p[0], p[1]]).collect();
    let mut fixed: Vec<Option<f64>> = vec![None; nv];
    let mut conflicting: Vec<(i64, f64)> = vec![];
    let mut redundant: Vec<i64> = vec![];
    let mut fix =
        |i: usize, v: f64, tag: i64, conflicting: &mut Vec<(i64, f64)>, red: &mut Vec<i64>| {
            if i >= nv {
                return;
            }
            match fixed[i] {
                Some(o) if (o - v).abs() > CHECK => conflicting.push((tag, (o - v).abs())),
                Some(_) => red.push(tag),
                None => {
                    fixed[i] = Some(v);
                }
            }
        };
    // pins first, then dragging: a dispute is resolved in favor of the pin
    for c in &sk.cons {
        match c.geo {
            Geo::Fix(p, a, b) => {
                fix(2 * p, a, c.tag, &mut conflicting, &mut redundant);
                fix(2 * p + 1, b, c.tag, &mut conflicting, &mut redundant);
            }
            Geo::FixX(p, a) => fix(2 * p, a, c.tag, &mut conflicting, &mut redundant),
            Geo::FixY(p, b) => fix(2 * p + 1, b, c.tag, &mut conflicting, &mut redundant),
            _ => {}
        }
    }
    for p in &opt.pinned {
        if 2 * p + 1 < nv {
            let (a, b) = (x[2 * p], x[2 * p + 1]);
            fix(2 * p, a, -1, &mut conflicting, &mut redundant);
            fix(2 * p + 1, b, -1, &mut conflicting, &mut redundant);
        }
    }
    conflicting.retain(|c| c.0 != -1);
    redundant.retain(|t| *t != -1);
    for (i, f) in fixed.iter().enumerate() {
        if let Some(v) = f {
            x[i] = *v;
        }
    }
    let bad_input = x.iter().any(|v| !v.is_finite())
        || sk.cons.iter().any(|c| match &c.geo {
            Geo::Dist(.., d)
            | Geo::DistX(.., d)
            | Geo::DistY(.., d)
            | Geo::PointLine(.., d)
            | Geo::LineDist(.., d)
            | Geo::Angle(.., d) => !d.is_finite(),
            _ => false,
        });
    let free: Vec<usize> = (0..nv).filter(|&i| fixed[i].is_none()).collect();
    let mut col_of = vec![usize::MAX; nv];
    for (c, &i) in free.iter().enumerate() {
        col_of[i] = c;
    }
    let eqs: Vec<&Con> = sk
        .cons
        .iter()
        .filter(|c| !matches!(c.geo, Geo::Fix(..) | Geo::FixX(..) | Geo::FixY(..)))
        .filter(|c| points_of(&c.geo).iter().all(|p| *p < sk.pts.len()))
        .collect();
    let cols: Vec<Vec<(usize, usize)>> = eqs
        .iter()
        .map(|c| {
            let mut v: Vec<(usize, usize)> = vec![];
            for p in points_of(&c.geo) {
                for i in [2 * p, 2 * p + 1] {
                    if col_of[i] != usize::MAX && !v.iter().any(|e| e.1 == i) {
                        v.push((col_of[i], i));
                    }
                }
            }
            v
        })
        .collect();
    let sys = Sys {
        cons: eqs,
        cols,
        n: free.len(),
    };
    if bad_input {
        let tags = sys.cons.iter().map(|c| (c.tag, f64::NAN)).collect();
        return Report {
            pts: sk.pts.clone(),
            converged: false,
            iterations: 0,
            residual: f64::NAN,
            dof: 0,
            free_points: vec![],
            redundant,
            conflicting: tags,
        };
    }
    let mut run = lm(&sys, &x, &free, opt);
    if worst(&run.rows) >= CHECK {
        // one more attempt from the least-squares solution, then the minimal conflict
        let again = lm(&sys, &run.x, &free, opt);
        let its = run.iterations + again.iterations;
        if worst(&again.rows) < worst(&run.rows) {
            run = again;
        }
        run.iterations = its;
    }
    let mut x = run.x;
    let rows = run.rows;
    let residual = worst(&rows);
    if residual >= CHECK {
        let start = x.clone();
        let set = min_conflict_set(sys.cons.len(), |idx| {
            worst(&lm(&sys.subset(idx), &start, &free, opt).rows) < CHECK
        });
        for k in set {
            conflicting.push((sys.cons[k].tag, worst(&rows[k..=k])));
        }
    }
    // rank and dependencies: row basis in constraint order
    let jb = sys.jac(&mut x);
    let mut space = RowSpace::default();
    for (k, crow) in jb.iter().enumerate() {
        let mut dependent = false;
        for row in crow {
            if !space.add(row) {
                dependent = true;
            }
        }
        if dependent {
            let tag = sys.cons[k].tag;
            if !redundant.contains(&tag) {
                redundant.push(tag);
            }
        }
    }
    let dof = sys.n - space.rank();
    let mut free_points: Vec<P> = vec![];
    for (c, &i) in free.iter().enumerate() {
        if 1.0 - space.coverage(c) > 1e-6 && !free_points.contains(&(i / 2)) {
            free_points.push(i / 2);
        }
    }
    free_points.sort_unstable();
    let mut merged: Vec<(i64, f64)> = vec![];
    for (t, r) in conflicting {
        match merged.iter_mut().find(|m| m.0 == t) {
            Some(m) => m.1 = m.1.max(r),
            None => merged.push((t, r)),
        }
    }
    Report {
        pts: x.chunks(2).map(|c| [c[0], c[1]]).collect(),
        converged: run.converged,
        iterations: run.iterations,
        residual,
        dof,
        free_points,
        redundant,
        conflicting: merged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rectangle p0..p3 counter-clockwise; p0 is pinned, sides are horizontal/vertical.
    fn rect(w: f64, h: f64) -> Sketch {
        let mut s = Sketch::default();
        for (x, y) in [(0.0, 0.0), (w, 0.1), (w + 0.2, h), (-0.1, h - 0.1)] {
            s.point(x, y);
        }
        s.add(1, Geo::Fix(0, 0.0, 0.0));
        s.add(2, Geo::Horizontal((0, 1)));
        s.add(3, Geo::Vertical((1, 2)));
        s.add(4, Geo::Horizontal((2, 3)));
        s.add(5, Geo::Vertical((3, 0)));
        s
    }

    #[test]
    fn rectangle_with_equal_and_dim_is_well_constrained() {
        let mut s = rect(1.7, 1.2);
        s.add(6, Geo::Dist(0, 1, 2.0));
        s.add(7, Geo::EqualLen((0, 1), (1, 2)));
        let r = solve(&s, &Options::default());
        assert_eq!(r.verdict(), Verdict::Well, "{r:?}");
        assert!(r.converged);
        let p2 = r.pts[2];
        assert!(
            (p2[0] - 2.0).abs() < 1e-8 && (p2[1] - 2.0).abs() < 1e-8,
            "{p2:?}"
        );
        assert_eq!(r.dof, 0);
        assert!(r.free_points.is_empty());
    }

    #[test]
    fn rectangle_without_height_is_under_constrained() {
        let mut s = rect(1.7, 1.2);
        s.add(6, Geo::Dist(0, 1, 2.0));
        let r = solve(&s, &Options::default());
        assert_eq!(r.verdict(), Verdict::Under);
        assert_eq!(r.dof, 1);
        // height is free: the top points move, the bottom ones are determined
        assert_eq!(r.free_points, vec![2, 3]);
    }

    #[test]
    fn rectangle_over_constrained_detected() {
        // consistent redundancy: the height is given both by an equality and by a dimension
        let mut s = rect(1.7, 1.2);
        s.add(6, Geo::Dist(0, 1, 2.0));
        s.add(7, Geo::EqualLen((0, 1), (1, 2)));
        s.add(8, Geo::Dist(1, 2, 2.0));
        let r = solve(&s, &Options::default());
        assert_eq!(r.verdict(), Verdict::Over);
        assert_eq!(r.redundant, vec![8]);
        // conflicting: the height dimension contradicts the equality
        let mut s = rect(1.7, 1.2);
        s.add(6, Geo::Dist(0, 1, 2.0));
        s.add(7, Geo::EqualLen((0, 1), (1, 2)));
        s.add(8, Geo::Dist(1, 2, 3.0));
        let r = solve(&s, &Options::default());
        assert_eq!(r.verdict(), Verdict::Conflict);
        assert!(r.redundant.contains(&8));
        // only the minimal set: height, equality, width; culprit first
        let tags: Vec<i64> = r.conflicting.iter().map(|c| c.0).collect();
        assert_eq!(tags, vec![8, 6, 7], "{r:?}");
    }

    #[test]
    fn fix_wins_over_pinned_and_zero_distance_is_not_redundant() {
        let mut s = Sketch::default();
        let a = s.point(1.0, 2.0);
        s.add(1, Geo::FixX(a, 0.0));
        let r = solve(
            &s,
            &Options {
                pinned: vec![a],
                ..Default::default()
            },
        );
        assert!(r.conflicting.is_empty() && r.redundant.is_empty(), "{r:?}");
        assert_eq!(r.pts[a], [0.0, 2.0]);
        // Dist 0: coincident points, two independent equations
        let mut s = Sketch::default();
        s.point(0.0, 0.0);
        s.point(0.3, -0.2);
        s.add(1, Geo::Fix(0, 0.0, 0.0));
        s.add(2, Geo::Dist(0, 1, 0.0));
        let r = solve(&s, &Options::default());
        assert_eq!(r.verdict(), Verdict::Well, "{r:?}");
        assert!(r.pts[1][0].abs() < 1e-9 && r.pts[1][1].abs() < 1e-9);
    }

    #[test]
    fn conflict_set_leaves_out_innocent_constraints() {
        // a is pinned; |ab| = 2 (1), |ac| = 1 (2), |ab| = 3 (3): the conflict is only 1 and 3
        let mut s = Sketch::default();
        s.point(0.0, 0.0);
        s.point(2.5, 0.0);
        s.point(0.0, 1.2);
        s.add(0, Geo::Fix(0, 0.0, 0.0));
        s.add(1, Geo::Dist(0, 1, 2.0));
        s.add(2, Geo::Dist(0, 2, 1.0));
        s.add(3, Geo::Dist(0, 1, 3.0));
        let r = solve(&s, &Options::default());
        let tags: Vec<i64> = r.conflicting.iter().map(|c| c.0).collect();
        assert_eq!(tags, vec![3, 1], "{r:?}");
    }

    #[test]
    fn double_fix_conflict_and_pinned_drag() {
        let mut s = Sketch::default();
        let a = s.point(0.0, 0.0);
        let b = s.point(3.0, 0.0);
        s.add(1, Geo::FixX(a, 0.0));
        s.add(2, Geo::FixX(a, 1.0));
        let r = solve(&s, &Options::default());
        assert_eq!(r.verdict(), Verdict::Conflict);
        assert_eq!(r.conflicting[0].0, 2);
        // drag b: the length holds, a moves minimally
        let mut s = Sketch::default();
        s.point(0.0, 0.0);
        s.point(6.0, 0.0);
        s.add(1, Geo::Dist(0, 1, 5.0));
        let r = solve(
            &s,
            &Options {
                pinned: vec![b],
                ..Default::default()
            },
        );
        assert!(r.converged);
        assert_eq!(r.pts[1], [6.0, 0.0]);
        assert!((r.pts[0][0] - 1.0).abs() < 1e-8);
    }

    #[test]
    fn angle_equal_gaps_and_midpoint() {
        // three vertical lines with the outer ones pinned: the middle one sits halfway
        let mut s = Sketch::default();
        for x in [0.0, 0.3, 2.0] {
            s.point(x, 1.0);
            s.point(x, -1.0);
        }
        for (k, x) in [(0, 0.0), (2, 2.0)] {
            s.add(1, Geo::Fix(2 * k, x, 1.0));
            s.add(1, Geo::Fix(2 * k + 1, x, -1.0));
        }
        s.add(2, Geo::FixY(2, 1.0));
        s.add(2, Geo::FixY(3, -1.0));
        s.add(3, Geo::Vertical((2, 3)));
        s.add(4, Geo::EqualGaps(vec![(0, 1), (2, 3), (4, 5)]));
        let r = solve(&s, &Options::default());
        assert!(r.converged, "{r:?}");
        assert!((r.pts[2][0] - 1.0).abs() < 1e-8);
        // inclined line at 30° to the horizontal, length 1
        let mut s = Sketch::default();
        s.point(0.0, 0.0);
        s.point(1.0, 0.0);
        s.point(0.0, 0.0);
        s.point(0.9, 0.2);
        s.add(1, Geo::Fix(0, 0.0, 0.0));
        s.add(1, Geo::Fix(1, 1.0, 0.0));
        s.add(1, Geo::Fix(2, 0.0, 0.0));
        s.add(2, Geo::Dist(2, 3, 1.0));
        s.add(3, Geo::Angle((0, 1), (2, 3), 30f64.to_radians()));
        let r = solve(&s, &Options::default());
        assert_eq!(r.verdict(), Verdict::Well, "{r:?}");
        assert!((r.pts[3][1] - 0.5).abs() < 1e-8);
        let mut s = Sketch::default();
        s.point(0.0, 0.0);
        s.point(4.0, 2.0);
        s.point(1.0, 1.0);
        s.add(1, Geo::Fix(0, 0.0, 0.0));
        s.add(1, Geo::Fix(1, 4.0, 2.0));
        s.add(2, Geo::Midpoint(2, (0, 1)));
        let r = solve(&s, &Options::default());
        assert_eq!(r.verdict(), Verdict::Well);
        assert!((r.pts[2][0] - 2.0).abs() < 1e-9 && (r.pts[2][1] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn non_finite_input_is_conflict_not_panic() {
        let mut s = Sketch::default();
        s.point(0.0, 0.0);
        s.point(1.0, 0.0);
        s.add(9, Geo::Dist(0, 1, f64::NAN));
        let r = solve(&s, &Options::default());
        assert_eq!(r.verdict(), Verdict::Conflict);
    }
}
