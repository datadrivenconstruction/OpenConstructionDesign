// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! ddc-solver — geometric constraints on plan points.
//! Method: damped Gauss–Newton with a minimum-norm step — for an underdetermined system
//! as few points as possible move (an edit "touches nothing extra").
//! Inconsistency: after convergence nonzero residuals remain — their constraints are the conflict.

/// Point index (variables x = 2i, y = 2i+1).
pub type P = usize;
/// Segment of two points.
pub type L = (P, P);

#[derive(Clone, Debug, PartialEq)]
pub enum Rule {
    /// |ab| = d.
    Dist(P, P, f64),
    /// Point is pinned.
    Fix(P),
    Parallel(L, L),
    Perp(L, L),
    /// Both points of the second segment lie on the line of the first.
    Collinear(L, L),
    /// Signed distance from line l1 to both points of l2 = d.
    LineDist(L, L, f64),
    /// Chain of lines: parallel, adjacent distances equal.
    Equal(Vec<L>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Constraint {
    /// Caller's key (model id) — used in the conflict report.
    pub tag: i64,
    pub rule: Rule,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    /// Tags of constraints that could not be satisfied, with their residuals.
    pub tags: Vec<(i64, f64)>,
}

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let v: Vec<String> = self
            .tags
            .iter()
            .map(|(t, r)| format!("#{t} (off by {r:.4})"))
            .collect();
        write!(f, "constraints conflict: {}", v.join(", "))
    }
}

pub const TOL: f64 = 1e-7;

/// Coordinates of point `p` in the variable vector.
pub fn pt(x: &[f64], p: P) -> (f64, f64) {
    (x[2 * p], x[2 * p + 1])
}

/// Vector of segment a→b.
pub fn dir(x: &[f64], l: L) -> (f64, f64) {
    let (a, b) = (pt(x, l.0), pt(x, l.1));
    (b.0 - a.0, b.1 - a.1)
}

/// Unit direction of a segment (degenerate case — no division by zero).
pub fn unit(x: &[f64], l: L) -> (f64, f64) {
    let d = dir(x, l);
    let n = d.0.hypot(d.1).max(1e-12);
    (d.0 / n, d.1 / n)
}

/// Angle in (−π, π].
pub fn wrap_angle(a: f64) -> f64 {
    let t = std::f64::consts::TAU;
    let r = (a + std::f64::consts::PI).rem_euclid(t) - std::f64::consts::PI;
    if r <= -std::f64::consts::PI {
        r + t
    } else {
        r
    }
}

/// Signed distance from line l to point p (left of the direction is positive).
pub fn sdist(x: &[f64], l: L, p: P) -> f64 {
    let u = unit(x, l);
    let (a, q) = (pt(x, l.0), pt(x, p));
    u.0 * (q.1 - a.1) - u.1 * (q.0 - a.0)
}

fn residuals(rule: &Rule, x: &[f64], out: &mut Vec<f64>) {
    match rule {
        Rule::Dist(a, b, d) => {
            let (p, q) = (pt(x, *a), pt(x, *b));
            out.push((q.0 - p.0).hypot(q.1 - p.1) - d);
        }
        Rule::Fix(_) => {}
        Rule::Parallel(l1, l2) => {
            let (u, v) = (unit(x, *l1), unit(x, *l2));
            out.push(u.0 * v.1 - u.1 * v.0);
        }
        Rule::Perp(l1, l2) => {
            let (u, v) = (unit(x, *l1), unit(x, *l2));
            out.push(u.0 * v.0 + u.1 * v.1);
        }
        Rule::Collinear(l1, l2) => {
            out.push(sdist(x, *l1, l2.0));
            out.push(sdist(x, *l1, l2.1));
        }
        Rule::LineDist(l1, l2, d) => {
            out.push(sdist(x, *l1, l2.0) - d);
            out.push(sdist(x, *l1, l2.1) - d);
        }
        Rule::Equal(ls) => {
            let gap = |i: usize| sdist(x, ls[i], ls[i + 1].0);
            for i in 1..ls.len().saturating_sub(1) {
                out.push(gap(i) - gap(0));
            }
            // the step is defined only between parallel lines
            for w in ls.windows(2) {
                let (u, v) = (unit(x, w[0]), unit(x, w[1]));
                out.push(u.0 * v.1 - u.1 * v.0);
            }
        }
    }
}

/// Points whose coordinates `residuals` of the rule read.
fn points(rule: &Rule, out: &mut Vec<P>) {
    match rule {
        Rule::Dist(a, b, _) => out.extend([*a, *b]),
        Rule::Fix(_) => {}
        Rule::Parallel(l1, l2)
        | Rule::Perp(l1, l2)
        | Rule::Collinear(l1, l2)
        | Rule::LineDist(l1, l2, _) => out.extend([l1.0, l1.1, l2.0, l2.1]),
        Rule::Equal(ls) => out.extend(ls.iter().flat_map(|l| [l.0, l.1])),
    }
}

fn all(cs: &[Constraint], x: &[f64]) -> (Vec<f64>, Vec<usize>) {
    let (mut r, mut owner) = (vec![], vec![]);
    for (k, c) in cs.iter().enumerate() {
        let n = r.len();
        residuals(&c.rule, x, &mut r);
        owner.extend(std::iter::repeat_n(k, r.len() - n));
    }
    (r, owner)
}

fn norm(r: &[f64]) -> f64 {
    r.iter().map(|v| v * v).sum::<f64>().sqrt()
}

/// Solves an m×m system (symmetric positive definite with damping) by Gaussian elimination
/// with pivoting; a degenerate column yields 0.
pub fn gauss(mut a: Vec<f64>, mut b: Vec<f64>, m: usize) -> Vec<f64> {
    for i in 0..m {
        let piv = (i..m)
            .max_by(|&p, &q| a[p * m + i].abs().total_cmp(&a[q * m + i].abs()))
            .unwrap_or(i);
        if piv != i {
            for j in 0..m {
                a.swap(i * m + j, piv * m + j);
            }
            b.swap(i, piv);
        }
        let d = a[i * m + i];
        if d.abs() < 1e-300 {
            continue;
        }
        for r in i + 1..m {
            let f = a[r * m + i] / d;
            if f != 0.0 {
                for j in i..m {
                    a[r * m + j] -= f * a[i * m + j];
                }
                b[r] -= f * b[i];
            }
        }
    }
    let mut y = vec![0.0; m];
    for i in (0..m).rev() {
        let s: f64 = (i + 1..m).map(|j| a[i * m + j] * y[j]).sum();
        let d = a[i * m + i];
        y[i] = if d.abs() < 1e-300 {
            0.0
        } else {
            (b[i] - s) / d
        };
    }
    y
}

/// Solution: `x` — point coordinates (2 per point), `pinned` — points that must not move
/// (just edited). Returns new coordinates or a conflict.
pub fn solve(cs: &[Constraint], x0: &[f64], pinned: &[P]) -> Result<Vec<f64>, Conflict> {
    let mut fixed = vec![false; x0.len()];
    let fixes: Vec<P> = cs
        .iter()
        .filter_map(|c| match c.rule {
            Rule::Fix(p) => Some(p),
            _ => None,
        })
        .collect();
    for &p in pinned.iter().chain(&fixes) {
        if 2 * p + 1 < fixed.len() {
            fixed[2 * p] = true;
            fixed[2 * p + 1] = true;
        }
    }
    let free: Vec<usize> = (0..x0.len()).filter(|&i| !fixed[i]).collect();
    let mut col = vec![usize::MAX; x0.len()];
    for (c, &i) in free.iter().enumerate() {
        col[i] = c;
    }
    // each rule reads only its own points: its Jacobian rows are zero elsewhere, so they are
    // built from its free variables alone (same numbers as differencing every rule)
    let vars: Vec<Vec<usize>> = cs
        .iter()
        .map(|c| {
            let mut ps = vec![];
            points(&c.rule, &mut ps);
            let mut v: Vec<usize> = ps
                .into_iter()
                .flat_map(|p| [2 * p, 2 * p + 1])
                .filter(|&i| i < x0.len() && !fixed[i])
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        })
        .collect();
    // first residual row of each rule (a rule always gives the same number of rows)
    let mut row0 = Vec::with_capacity(cs.len());
    let (mut rp, mut rm) = (vec![], vec![]);
    for c in cs {
        row0.push(rp.len());
        residuals(&c.rule, x0, &mut rp);
    }
    let mut x = x0.to_vec();
    let scale = x0.iter().fold(1.0f64, |m, v| m.max(v.abs()));
    for _ in 0..100 {
        let (r, _) = all(cs, &x);
        let rn = norm(&r);
        if r.iter().all(|v| v.abs() < TOL) || free.is_empty() {
            break;
        }
        let m = r.len();
        let n = free.len();
        let h = 1e-7 * scale;
        // sparse Jacobian by column: (row, value), rows ascending
        let mut by_col: Vec<Vec<(usize, f64)>> = vec![vec![]; n];
        for (k, c) in cs.iter().enumerate() {
            for &i in &vars[k] {
                let old = x[i];
                x[i] = old + h;
                rp.clear();
                residuals(&c.rule, &x, &mut rp);
                x[i] = old - h;
                rm.clear();
                residuals(&c.rule, &x, &mut rm);
                x[i] = old;
                for (j, (p, q)) in rp.iter().zip(&rm).enumerate() {
                    let v = (p - q) / (2.0 * h);
                    if v != 0.0 {
                        by_col[col[i]].push((row0[k] + j, v));
                    }
                }
            }
        }
        // minimum-norm step (J J^T + lambda I) y = -r, dx = J^T y; the sums run over the
        // columns in order, as the dense products did
        let mut jj = vec![0.0; m * m];
        for list in &by_col {
            for &(a, va) in list {
                for &(b, vb) in list {
                    jj[a * m + b] += va * vb;
                }
            }
        }
        for a in 0..m {
            jj[a * m + a] += 1e-10;
        }
        let y = gauss(jj, r.iter().map(|v| -v).collect(), m);
        let dx: Vec<f64> = by_col
            .iter()
            .map(|list| list.iter().map(|&(k, v)| v * y[k]).sum())
            .collect();
        let mut t = 1.0;
        let mut ok = false;
        while t > 1e-6 {
            let mut xt = x.clone();
            for (c, &i) in free.iter().enumerate() {
                xt[i] += t * dx[c];
            }
            if norm(&all(cs, &xt).0) < rn {
                x = xt;
                ok = true;
                break;
            }
            t *= 0.5;
        }
        if !ok {
            break;
        }
    }
    let (r, owner) = all(cs, &x);
    let mut bad: Vec<(i64, f64)> = vec![];
    for (k, v) in r.iter().enumerate() {
        if v.abs() >= 1e-6 {
            let tag = cs[owner[k]].tag;
            match bad.iter_mut().find(|b| b.0 == tag) {
                Some(b) => b.1 = b.1.max(v.abs()),
                None => bad.push((tag, v.abs())),
            }
        }
    }
    if bad.is_empty() {
        Ok(x)
    } else {
        Err(Conflict { tags: bad })
    }
}

/// Row space of the Jacobian: orthonormal basis (modified Gram–Schmidt, two passes).
/// A row lying almost entirely in the basis is dependent (a redundant equation).
#[derive(Clone, Debug, Default)]
pub struct RowSpace {
    basis: Vec<Vec<f64>>,
}

impl RowSpace {
    /// Adds a row; false — the row is zero or depends on rows already added.
    pub fn add(&mut self, row: &[f64]) -> bool {
        let n0 = row.iter().map(|v| v * v).sum::<f64>().sqrt();
        if n0 < 1e-12 {
            return false;
        }
        let mut v: Vec<f64> = row.iter().map(|a| a / n0).collect();
        for _ in 0..2 {
            for b in &self.basis {
                let d: f64 = v.iter().zip(b).map(|(a, c)| a * c).sum();
                v.iter_mut().zip(b).for_each(|(a, c)| *a -= d * c);
            }
        }
        let nn = v.iter().map(|a| a * a).sum::<f64>().sqrt();
        if nn < 1e-7 {
            return false;
        }
        v.iter_mut().for_each(|a| *a /= nn);
        self.basis.push(v);
        true
    }

    pub fn rank(&self) -> usize {
        self.basis.len()
    }

    /// Fraction of column `c` (a variable's unit vector) in the row space: 1 — the variable
    /// is determined by the equations, less — it has freedom.
    pub fn coverage(&self, c: usize) -> f64 {
        self.basis
            .iter()
            .map(|b| b.get(c).map_or(0.0, |v| v * v))
            .sum()
    }
}

/// Minimal inconsistent subset of `m` equations (indices; culprit first, then in order):
/// the culprit is the first equation after which the prefix is inconsistent (binary search
/// over the monotone prefix); a deletion filter then keeps only the equations without which
/// the set with the culprit becomes consistent. Empty result — everything is consistent.
pub fn min_conflict_set(m: usize, mut feasible: impl FnMut(&[usize]) -> bool) -> Vec<usize> {
    let all: Vec<usize> = (0..m).collect();
    if m == 0 || feasible(&all) {
        return vec![];
    }
    let (mut lo, mut hi) = (0usize, m - 1);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if feasible(&all[..=mid]) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let k = lo;
    let mut keep: Vec<usize> = (0..k).collect();
    let mut j = keep.len();
    while j > 0 {
        j -= 1;
        let drop = keep[j];
        let mut trial: Vec<usize> = keep.iter().copied().filter(|&x| x != drop).collect();
        trial.push(k);
        if !feasible(&trial) {
            keep.remove(j);
        }
    }
    let mut out = vec![k];
    out.extend(keep);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(tag: i64, rule: Rule) -> Constraint {
        Constraint { tag, rule }
    }

    #[test]
    fn locked_length_follows_dragged_end() {
        // segment 0→1 of length 5; drag point 1 — length holds, point 0 moves minimally
        let x = [0.0, 0.0, 6.0, 0.0];
        let s = solve(&[c(1, Rule::Dist(0, 1, 5.0))], &x, &[1]).unwrap();
        assert!(((s[2] - s[0]).hypot(s[3] - s[1]) - 5.0).abs() < 1e-9);
        assert_eq!((s[2], s[3]), (6.0, 0.0));
    }

    #[test]
    fn equal_chain_and_perpendicular() {
        // three vertical lines x=0, x=2, x=5 → equal steps, first and last pinned
        let x = [0.0, 0.0, 0.0, 4.0, 2.0, 0.0, 2.0, 4.0, 5.0, 0.0, 5.0, 4.0];
        let cs = [
            c(1, Rule::Equal(vec![(0, 1), (2, 3), (4, 5)])),
            c(2, Rule::Parallel((0, 1), (2, 3))),
        ];
        let s = solve(&cs, &x, &[0, 1, 4, 5]).unwrap();
        assert!((s[4] - 2.5).abs() < 1e-7 && (s[6] - 2.5).abs() < 1e-7);
        let y = [0.0, 0.0, 4.0, 0.0, 1.0, 1.0, 1.5, 3.0];
        let s = solve(&[c(3, Rule::Perp((0, 1), (2, 3)))], &y, &[0, 1, 2]).unwrap();
        assert!((s[6] - 1.0).abs() < 1e-7);
    }

    #[test]
    fn conflict_names_the_rules() {
        let x = [0.0, 0.0, 3.0, 0.0];
        let e = solve(&[c(7, Rule::Dist(0, 1, 5.0)), c(8, Rule::Fix(0))], &x, &[1]).unwrap_err();
        assert_eq!(e.tags.len(), 1);
        assert_eq!(e.tags[0].0, 7);
        assert!(e.to_string().contains("#7"));
    }

    #[test]
    fn row_space_and_minimal_conflict() {
        let mut rs = RowSpace::default();
        assert!(rs.add(&[1.0, 0.0, 0.0]));
        assert!(!rs.add(&[2.0, 0.0, 0.0]));
        assert!(rs.add(&[1.0, 1.0, 0.0]));
        assert_eq!(rs.rank(), 2);
        assert!((rs.coverage(1) - 1.0).abs() < 1e-12 && rs.coverage(2) < 1e-12);
        // x = 1 (0), y = 2 (1), x = 3 (2), z = 0 (3): only 0 and 2 conflict
        let eqs = [(0usize, 1.0), (1, 2.0), (0, 3.0), (2, 0.0)];
        let feasible = |set: &[usize]| {
            let mut v: [Option<f64>; 3] = [None; 3];
            set.iter().all(|&i| {
                let (var, val) = eqs[i];
                match v[var] {
                    Some(o) => (o - val).abs() < 1e-12,
                    None => {
                        v[var] = Some(val);
                        true
                    }
                }
            })
        };
        assert_eq!(min_conflict_set(4, feasible), vec![2, 0]);
        assert!(min_conflict_set(2, |_: &[usize]| true).is_empty());
        assert!((wrap_angle(3.0 * std::f64::consts::PI) - std::f64::consts::PI).abs() < 1e-12);
    }
}
