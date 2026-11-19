// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Profile (skyline) LDL^T storage, factorization and solves; reverse Cuthill-McKee ordering.
//! Row i keeps columns first[i]..=i of the lower triangle; after `factor` it holds L and D.

/// Reverse Cuthill-McKee node order (order[k] = node); deterministic, per connected component.
pub fn rcm(adj: &[Vec<usize>]) -> Vec<usize> {
    let n = adj.len();
    let deg: Vec<usize> = adj.iter().map(|a| a.len()).collect();
    let mut by_deg: Vec<usize> = (0..n).collect();
    by_deg.sort_by_key(|&v| (deg[v], v));
    let mut seen = vec![false; n];
    let mut mark = vec![0u32; n];
    let mut stamp = 0u32;
    let mut order = Vec::with_capacity(n);
    for &start in &by_deg {
        if seen[start] {
            continue;
        }
        // Pseudo-peripheral root: walk to a min-degree node of the last BFS level.
        let mut root = start;
        stamp += 1;
        let (mut ecc, mut last) = bfs_last(root, adj, &mut mark, stamp);
        for _ in 0..8 {
            let cand = *last.iter().min_by_key(|&&v| (deg[v], v)).unwrap_or(&root);
            stamp += 1;
            let (e, l) = bfs_last(cand, adj, &mut mark, stamp);
            if e > ecc {
                root = cand;
                ecc = e;
                last = l;
            } else {
                break;
            }
        }
        let head = order.len();
        seen[root] = true;
        order.push(root);
        let mut k = head;
        while k < order.len() {
            let v = order[k];
            k += 1;
            let mut nb: Vec<usize> = adj[v].iter().copied().filter(|&w| !seen[w]).collect();
            nb.sort_by_key(|&w| (deg[w], w));
            for w in nb {
                seen[w] = true;
                order.push(w);
            }
        }
    }
    order.reverse();
    order
}

/// BFS from `root`: (depth, nodes of the last level).
fn bfs_last(root: usize, adj: &[Vec<usize>], mark: &mut [u32], stamp: u32) -> (usize, Vec<usize>) {
    let mut front = vec![root];
    mark[root] = stamp;
    let mut depth = 0;
    loop {
        let mut next = Vec::new();
        for &v in &front {
            for &w in &adj[v] {
                if mark[w] != stamp {
                    mark[w] = stamp;
                    next.push(w);
                }
            }
        }
        if next.is_empty() {
            return (depth, front);
        }
        depth += 1;
        front = next;
    }
}

pub struct Skyline {
    first: Vec<usize>,
    start: Vec<usize>,
    a: Vec<f64>,
}

impl Skyline {
    pub fn new(first: Vec<usize>) -> Self {
        let mut start = Vec::with_capacity(first.len());
        let mut len = 0;
        for (i, &f) in first.iter().enumerate() {
            start.push(len);
            len += i - f + 1;
        }
        Skyline { first, start, a: vec![0.0; len] }
    }

    pub fn n(&self) -> usize {
        self.first.len()
    }

    pub fn profile(&self) -> usize {
        self.a.len()
    }

    fn at(&self, i: usize, j: usize) -> usize {
        self.start[i] + (j - self.first[i])
    }

    /// Adds `v` at (i, j) of the symmetric matrix; (i, j) must lie inside the profile.
    pub fn add(&mut self, i: usize, j: usize, v: f64) {
        let (r, c) = if i >= j { (i, j) } else { (j, i) };
        let k = self.at(r, c);
        self.a[k] += v;
    }

    /// In-place LDL^T; Err(i) when pivot i is not above `tol` * its original diagonal.
    pub fn factor(&mut self, tol: f64) -> Result<(), usize> {
        let n = self.n();
        let diag0: Vec<f64> = (0..n).map(|i| self.a[self.at(i, i)]).collect();
        let dmax = diag0.iter().fold(0.0f64, |m, &v| m.max(v.abs()));
        for i in 0..n {
            let fi = self.first[i];
            let si = self.start[i];
            for j in fi..i {
                let fj = self.first[j];
                let sj = self.start[j];
                let k0 = fi.max(fj);
                let s: f64 = {
                    let ri = &self.a[si + (k0 - fi)..si + (j - fi)];
                    let rj = &self.a[sj + (k0 - fj)..sj + (j - fj)];
                    ri.iter().zip(rj).map(|(x, y)| x * y).sum()
                };
                self.a[si + (j - fi)] -= s;
            }
            // Row i now holds u_ij = l_ij * d_j.
            let mut d = self.a[si + (i - fi)];
            for j in fi..i {
                let dj = self.a[self.start[j] + (j - self.first[j])];
                let u = self.a[si + (j - fi)];
                let l = u / dj;
                d -= l * u;
                self.a[si + (j - fi)] = l;
            }
            let ok = diag0[i] > dmax * 1e-15 && d > tol * diag0[i] && d.is_finite();
            if !ok {
                return Err(i);
            }
            self.a[si + (i - fi)] = d;
        }
        Ok(())
    }

    /// Solves (L D L^T) x = b in place after `factor`.
    pub fn solve(&self, b: &mut [f64]) {
        let n = self.n();
        for i in 0..n {
            let fi = self.first[i];
            let si = self.start[i];
            let row = &self.a[si..si + (i - fi)];
            let s: f64 = row.iter().zip(&b[fi..i]).map(|(l, x)| l * x).sum();
            b[i] -= s;
        }
        for i in 0..n {
            b[i] /= self.a[self.at(i, i)];
        }
        for i in (0..n).rev() {
            let fi = self.first[i];
            let si = self.start[i];
            let x = b[i];
            for (k, l) in (fi..i).zip(&self.a[si..si + (i - fi)]) {
                b[k] -= l * x;
            }
        }
    }
}
