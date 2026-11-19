//! Uniform cell grid over triangle bounding boxes: finds the triangle under a point and
//! segment candidates in O(1) per cell.

use crate::math::{V2, V3};

#[derive(Clone, Debug, Default)]
pub(crate) struct Index {
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    cells: Vec<Vec<u32>>,
}

impl Index {
    pub fn new(pts: &[V3], tris: &[[u32; 3]]) -> Index {
        if tris.is_empty() {
            return Index::default();
        }
        let (mut lo, mut hi) = (
            V2 {
                x: f64::MAX,
                y: f64::MAX,
            },
            V2 {
                x: f64::MIN,
                y: f64::MIN,
            },
        );
        for p in pts {
            lo = V2 {
                x: lo.x.min(p.x),
                y: lo.y.min(p.y),
            };
            hi = V2 {
                x: hi.x.max(p.x),
                y: hi.y.max(p.y),
            };
        }
        let (w, h) = ((hi.x - lo.x).max(1e-9), (hi.y - lo.y).max(1e-9));
        let n = (tris.len() as f64 / 2.0).clamp(1.0, 1.0e6);
        let cell = (w * h / n).sqrt().max(w.max(h) / 4096.0).max(1e-9);
        let nx = ((w / cell).floor() as usize + 1).min(4096);
        let ny = ((h / cell).floor() as usize + 1).min(4096);
        let mut ix = Index {
            x0: lo.x,
            y0: lo.y,
            cell,
            nx,
            ny,
            cells: vec![vec![]; nx * ny],
        };
        for (t, tr) in tris.iter().enumerate() {
            let [a, b, c] = tr.map(|i| pts[i as usize]);
            let (i0, j0) = ix.cell_of(a.x.min(b.x).min(c.x), a.y.min(b.y).min(c.y));
            let (i1, j1) = ix.cell_of(a.x.max(b.x).max(c.x), a.y.max(b.y).max(c.y));
            if (i1 - i0 + 1) * (j1 - j0 + 1) <= 4 {
                for j in j0..=j1 {
                    for i in i0..=i1 {
                        ix.cells[j * nx + i].push(t as u32);
                    }
                }
                continue;
            }
            // long triangles (slivers along the shell) cover only the strip cells they touch
            let tri = [a, b, c].map(|p| V2 { x: p.x, y: p.y });
            for j in j0..=j1 {
                let ya = ix.y0 + j as f64 * ix.cell;
                let yb = if j + 1 == ny { f64::MAX } else { ya + ix.cell };
                let Some((xa, xb)) = strip_x(&tri, ya, yb) else {
                    continue;
                };
                let (ia, _) = ix.cell_of(xa, ya);
                let (ib, _) = ix.cell_of(xb, ya);
                for i in ia.max(i0)..=ib.min(i1) {
                    ix.cells[j * nx + i].push(t as u32);
                }
            }
        }
        ix
    }

    fn cell_of(&self, x: f64, y: f64) -> (usize, usize) {
        let i = ((x - self.x0) / self.cell)
            .floor()
            .clamp(0.0, (self.nx - 1) as f64) as usize;
        let j = ((y - self.y0) / self.cell)
            .floor()
            .clamp(0.0, (self.ny - 1) as f64) as usize;
        (i, j)
    }

    /// Triangles of the point's cell (candidates; the point may lie outside all of them).
    pub fn at(&self, p: V2) -> &[u32] {
        if self.cells.is_empty() || !p.x.is_finite() || !p.y.is_finite() {
            return &[];
        }
        let (i, j) = self.cell_of(p.x, p.y);
        &self.cells[j * self.nx + i]
    }

    /// Triangles of the rectangle's cells, without duplicates.
    pub fn rect(&self, lo: V2, hi: V2) -> Vec<u32> {
        if self.cells.is_empty() {
            return vec![];
        }
        let (i0, j0) = self.cell_of(lo.x, lo.y);
        let (i1, j1) = self.cell_of(hi.x, hi.y);
        let mut v = vec![];
        for j in j0..=j1 {
            for i in i0..=i1 {
                v.extend_from_slice(&self.cells[j * self.nx + i]);
            }
        }
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// x-range of the triangle's intersection with the strip ya ≤ y ≤ yb (with rounding slack).
fn strip_x(t: &[V2; 3], ya: f64, yb: f64) -> Option<(f64, f64)> {
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    let mut add = |x: f64| {
        lo = lo.min(x);
        hi = hi.max(x);
    };
    for k in 0..3 {
        let (p, q) = (t[k], t[(k + 1) % 3]);
        if p.y >= ya && p.y <= yb {
            add(p.x);
        }
        for y in [ya, yb] {
            if (p.y - y) * (q.y - y) < 0.0 {
                add(p.x + (q.x - p.x) * (y - p.y) / (q.y - p.y));
            }
        }
    }
    let pad = 1e-9 * (1.0 + lo.abs().max(hi.abs()));
    (lo <= hi).then(|| (lo - pad, hi + pad))
}
