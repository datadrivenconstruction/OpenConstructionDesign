// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Uniform grid of bounding boxes for neighbour search: a query returns candidates in ascending
//! index order (a superset of the intersecting boxes), so a "first match" scan gives the same
//! result as a full brute-force pass. Non-finite boxes are returned by every query.

/// Bounding box [xmin, ymin, xmax, ymax].
pub(crate) type Bbox = [f64; 4];

pub(crate) struct BoxGrid {
    x0: f64,
    y0: f64,
    inv: f64,
    nx: usize,
    ny: usize,
    cells: Vec<Vec<u32>>,
    always: Vec<u32>,
}

/// A box larger than the median by this factor is served by the `always` list.
const OUTLIER_K: f64 = 16.0;

fn finite(b: &Bbox) -> bool {
    b.iter().all(|v| v.is_finite()) && b[0] <= b[2] && b[1] <= b[3]
}

impl BoxGrid {
    pub(crate) fn new(boxes: &[Bbox]) -> Self {
        let mut ext = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        let (mut sum, mut cnt) = (0.0, 0usize);
        let ext_of = |b: &Bbox| (b[2] - b[0]).max(b[3] - b[1]);
        // Boxes much larger than the median do not inflate the cell size: they go to `always`.
        let mut sizes: Vec<f64> = boxes.iter().filter(|b| finite(b)).map(ext_of).collect();
        sizes.sort_unstable_by(f64::total_cmp);
        let med = sizes.get(sizes.len() / 2).copied().unwrap_or(0.0);
        let big = if med > 0.0 {
            med * OUTLIER_K
        } else {
            f64::INFINITY
        };
        let outlier = |b: &Bbox| finite(b) && ext_of(b) > big;
        for b in boxes.iter().filter(|b| finite(b) && !outlier(b)) {
            ext = [
                ext[0].min(b[0]),
                ext[1].min(b[1]),
                ext[2].max(b[2]),
                ext[3].max(b[3]),
            ];
            sum += ext_of(b);
            cnt += 1;
        }
        let mut g = BoxGrid {
            x0: 0.0,
            y0: 0.0,
            inv: 0.0,
            nx: 1,
            ny: 1,
            cells: vec![vec![]],
            always: vec![],
        };
        if cnt > 0 {
            let (w, h) = (ext[2] - ext[0], ext[3] - ext[1]);
            // The cell is at least the mean box size, with at most ~4 cells per box.
            let mut cell = (sum / cnt as f64).max((w * h / cnt as f64).sqrt());
            let limit = (4 * cnt + 16) as f64;
            if cell.is_nan()
                || cell <= 0.0
                || ((w / cell).ceil() + 1.0) * ((h / cell).ceil() + 1.0) > limit
            {
                cell = cell.max((w.max(h) / limit.sqrt()).max(f64::MIN_POSITIVE) * 1.01);
            }
            if cell.is_finite() && cell > 0.0 {
                g.x0 = ext[0];
                g.y0 = ext[1];
                g.inv = 1.0 / cell;
                g.nx = ((w * g.inv) as usize + 1).min(1 << 15);
                g.ny = ((h * g.inv) as usize + 1).min(1 << 15);
                g.cells = vec![vec![]; g.nx * g.ny];
            }
        }
        for (k, b) in boxes.iter().enumerate() {
            if !finite(b) || outlier(b) || g.inv == 0.0 {
                g.always.push(k as u32);
                continue;
            }
            let (i0, j0, i1, j1) = g.span(b);
            for j in j0..=j1 {
                for i in i0..=i1 {
                    g.cells[j * g.nx + i].push(k as u32);
                }
            }
        }
        g
    }

    fn span(&self, b: &Bbox) -> (usize, usize, usize, usize) {
        let ix = |x: f64| (((x - self.x0) * self.inv).max(0.0) as usize).min(self.nx - 1);
        let iy = |y: f64| (((y - self.y0) * self.inv).max(0.0) as usize).min(self.ny - 1);
        (ix(b[0]), iy(b[1]), ix(b[2]), iy(b[3]))
    }

    /// Candidates for box `b` into `out` (cleared first): ascending, without duplicates.
    pub(crate) fn query(&self, b: &Bbox, out: &mut Vec<usize>) {
        out.clear();
        out.extend(self.always.iter().map(|&k| k as usize));
        if self.inv != 0.0 && b.iter().all(|v| !v.is_nan()) {
            let (i0, j0, i1, j1) = self.span(b);
            for j in j0..=j1 {
                for i in i0..=i1 {
                    out.extend(self.cells[j * self.nx + i].iter().map(|&k| k as usize));
                }
            }
        } else if self.inv != 0.0 {
            out.extend(self.cells.iter().flatten().map(|&k| k as usize));
        }
        out.sort_unstable();
        out.dedup();
    }
}

/// Bounding box of the points, expanded by `pad` plus a rounding margin.
pub(crate) fn bbox_of(pts: &[crate::V2], pad: f64) -> Bbox {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for p in pts {
        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
    }
    let m = b.iter().fold(1.0f64, |m, v| m.max(v.abs()));
    let pad = pad + m * 1e-12;
    [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_is_superset_and_sorted() {
        let boxes: Vec<Bbox> = (0..200)
            .map(|k| {
                let x = (k * 37 % 101) as f64;
                let y = (k * 53 % 97) as f64;
                [x, y, x + (k % 7) as f64, y + 1.0]
            })
            .chain([[f64::NAN, 0.0, 1.0, 1.0]])
            .collect();
        let g = BoxGrid::new(&boxes);
        let mut out = vec![];
        for q in [
            [10.0, 10.0, 12.0, 30.0],
            [-5.0, -5.0, -1.0, -1.0],
            [50.0, 50.0, 50.0, 50.0],
        ] {
            g.query(&q, &mut out);
            assert!(out.windows(2).all(|w| w[0] < w[1]));
            assert!(out.contains(&200));
            for (k, b) in boxes.iter().enumerate().take(200) {
                if b[0] <= q[2] && q[0] <= b[2] && b[1] <= q[3] && q[1] <= b[3] {
                    assert!(out.contains(&k), "{k}");
                }
            }
        }
    }

    #[test]
    fn huge_outlier_goes_to_always_and_keeps_cells_small() {
        let mut boxes: Vec<Bbox> = (0..10_000)
            .map(|k| {
                let x = (k % 100) as f64 * 5.0;
                let y = (k / 100) as f64 * 5.0;
                [x, y, x + 1.0, y + 1.0]
            })
            .collect();
        let plain = BoxGrid::new(&boxes);
        boxes.push([0.0, 0.0, 1e9, 1e-3]);
        let g = BoxGrid::new(&boxes);
        assert_eq!(g.always, [10_000]);
        assert!(g.inv >= plain.inv * 0.99, "{} vs {}", g.inv, plain.inv);
        let mut out = vec![];
        g.query(&[0.0, 0.0, 1.0, 1.0], &mut out);
        assert!(out.contains(&10_000) && out.contains(&0));
    }
}
