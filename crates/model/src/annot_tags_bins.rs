//! Obstacles of tag placement in a uniform grid: a query tests only what shares a cell with the
//! candidate, so tagging thousands of openings stays near linear.

use std::collections::HashMap;

use ddc_geom::{v2, V2};

/// Cell side, m: a few tag sizes, so a candidate touches one to four cells.
const CELL: f64 = 2.0;
/// A box or wall wider than this many cells is kept in a list every query reads.
const WIDE: i64 = 64;

#[derive(Default)]
struct Bins {
    map: HashMap<(i64, i64), Vec<u32>>,
    wide: Vec<u32>,
}

fn cell(x: f64) -> i64 {
    (x / CELL).floor() as i64
}

impl Bins {
    fn insert(&mut self, i: u32, lo: V2, hi: V2) {
        let (x0, x1, y0, y1) = (cell(lo.x), cell(hi.x), cell(lo.y), cell(hi.y));
        if x1 - x0 > WIDE || y1 - y0 > WIDE || !(lo.x.is_finite() && hi.y.is_finite()) {
            self.wide.push(i);
            return;
        }
        for x in x0..=x1 {
            for y in y0..=y1 {
                self.map.entry((x, y)).or_default().push(i);
            }
        }
    }

    /// Entries whose cells meet [lo, hi], each once, in insertion order.
    fn near(&self, lo: V2, hi: V2, out: &mut Vec<u32>) {
        out.clear();
        out.extend_from_slice(&self.wide);
        for x in cell(lo.x)..=cell(hi.x) {
            for y in cell(lo.y)..=cell(hi.y) {
                if let Some(v) = self.map.get(&(x, y)) {
                    out.extend_from_slice(v);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
    }
}

/// Placed tag boxes and wall axes with their half thickness.
#[derive(Default)]
pub(super) struct Obstacles {
    boxes: Vec<(V2, V2)>,
    box_bins: Bins,
    walls: Vec<(V2, V2, f64)>,
    wall_bins: Bins,
}

impl Obstacles {
    pub(super) fn new(walls: Vec<(V2, V2, f64)>) -> Self {
        let mut o = Obstacles::default();
        for (i, &(a, b, half)) in walls.iter().enumerate() {
            let lo = v2(a.x.min(b.x) - half, a.y.min(b.y) - half);
            let hi = v2(a.x.max(b.x) + half, a.y.max(b.y) + half);
            o.wall_bins.insert(i as u32, lo, hi);
        }
        o.walls = walls;
        o
    }

    pub(super) fn add_box(&mut self, b: (V2, V2)) {
        self.box_bins.insert(self.boxes.len() as u32, b.0, b.1);
        self.boxes.push(b);
    }

    /// No placed tag box overlaps [lo, hi].
    pub(super) fn clear_of_tags(&self, lo: V2, hi: V2, buf: &mut Vec<u32>) -> bool {
        self.box_bins.near(lo, hi, buf);
        !buf.iter().any(|&i| {
            let b = &self.boxes[i as usize];
            lo.x < b.1.x && hi.x > b.0.x && lo.y < b.1.y && hi.y > b.0.y
        })
    }

    /// No wall crosses [lo, hi] grown by its half thickness.
    pub(super) fn clear_of_walls(&self, lo: V2, hi: V2, buf: &mut Vec<u32>) -> bool {
        self.wall_bins.near(lo, hi, buf);
        !buf.iter().any(|&i| {
            let (a, b, half) = self.walls[i as usize];
            super::seg_hits(a, b, lo - v2(half, half), hi + v2(half, half))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid answers as a scan of every obstacle would.
    #[test]
    fn grid_matches_a_full_scan() {
        let mut walls = vec![];
        for k in 0..40 {
            let t = k as f64 * 1.7;
            walls.push((v2(t, -3.0), v2(t + 0.3, 30.0), 0.1));
            walls.push((v2(-5.0, t), v2(200.0, t), 0.15));
        }
        let mut o = Obstacles::new(walls.clone());
        let mut boxes = vec![];
        for k in 0..300 {
            let c = v2((k * 37 % 97) as f64 * 0.7, (k * 53 % 89) as f64 * 0.4);
            let b = (c - v2(0.4, 0.2), c + v2(0.4, 0.2));
            o.add_box(b);
            boxes.push(b);
        }
        let mut buf = vec![];
        for k in 0..2000 {
            let c = v2(
                (k * 13 % 211) as f64 * 0.31 - 3.0,
                (k * 7 % 173) as f64 * 0.19 - 2.0,
            );
            let (lo, hi) = (c - v2(0.5, 0.25), c + v2(0.5, 0.25));
            let tags = !boxes
                .iter()
                .any(|b| lo.x < b.1.x && hi.x > b.0.x && lo.y < b.1.y && hi.y > b.0.y);
            let ws = !walls
                .iter()
                .any(|&(a, b, h)| super::super::seg_hits(a, b, lo - v2(h, h), hi + v2(h, h)));
            assert_eq!(o.clear_of_tags(lo, hi, &mut buf), tags, "{k}");
            assert_eq!(o.clear_of_walls(lo, hi, &mut buf), ws, "{k}");
        }
    }
}
