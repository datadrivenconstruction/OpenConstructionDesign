// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Bounding-box index for plan picking, cached by (level, revision) like the snap index.
//! Generic over the item kind: the owner supplies one box per item (position = item index) and
//! runs its exact test only on the candidates, so the answer equals a full scan.

use ddc_geom::V2;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const CELL: f64 = 4.0;
/// Boxes spanning more cells than this live in a flat list instead of the buckets.
const MAX_CELLS: i64 = 256;

/// Axis-aligned box: (min x, min y, max x, max y).
pub type Bx = (f64, f64, f64, f64);

/// Box of points, grown by `pad` on every side.
pub fn bx_of(pts: impl IntoIterator<Item = V2>, pad: f64) -> Option<Bx> {
    let mut it = pts.into_iter();
    let p = it.next()?;
    let mut b = (p.x, p.y, p.x, p.y);
    for p in it {
        b = (b.0.min(p.x), b.1.min(p.y), b.2.max(p.x), b.3.max(p.y));
    }
    Some((b.0 - pad, b.1 - pad, b.2 + pad, b.3 + pad))
}

fn cell(v: f64) -> i64 {
    (v / CELL).floor() as i64
}

#[derive(Default)]
pub struct BoxIndex {
    boxes: Vec<Option<Bx>>,
    cells: HashMap<(i64, i64), Vec<u32>>,
    long: Vec<u32>,
}

impl BoxIndex {
    /// `boxes[i]` is item `i`; `None` items are never offered.
    pub fn build(boxes: Vec<Option<Bx>>) -> Self {
        let mut ix = BoxIndex {
            boxes,
            ..Default::default()
        };
        for (i, b) in ix.boxes.iter().enumerate() {
            let Some(b) = b else { continue };
            let (x0, x1, y0, y1) = (cell(b.0), cell(b.2), cell(b.1), cell(b.3));
            if (x1 - x0 + 1).saturating_mul(y1 - y0 + 1) > MAX_CELLS {
                ix.long.push(i as u32);
                continue;
            }
            for x in x0..=x1 {
                for y in y0..=y1 {
                    ix.cells.entry((x, y)).or_default().push(i as u32);
                }
            }
        }
        ix
    }

    /// Item indices whose box lies within `reach` of `p`, ascending (the order of a linear scan).
    pub fn near(&self, p: V2, reach: f64) -> Vec<u32> {
        let mut ids: Vec<u32> = self.long.clone();
        for x in cell(p.x - reach)..=cell(p.x + reach) {
            for y in cell(p.y - reach)..=cell(p.y + reach) {
                if let Some(v) = self.cells.get(&(x, y)) {
                    ids.extend_from_slice(v);
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.retain(|&i| {
            self.boxes[i as usize].is_some_and(|b| {
                p.x >= b.0 - reach && p.x <= b.2 + reach && p.y >= b.1 - reach && p.y <= b.3 + reach
            })
        });
        ids
    }
}

/// Editor-side holder: interior mutability so pick paths keep `&Editor`.
#[derive(Default)]
pub struct PickCache {
    slot: Mutex<Option<((i64, u64), BoxIndex)>>,
    /// How many times the index was rebuilt.
    pub builds: AtomicUsize,
}

impl PickCache {
    pub fn with<R>(
        &self,
        key: (i64, u64),
        load: impl FnOnce() -> BoxIndex,
        f: impl FnOnce(&BoxIndex) -> R,
    ) -> R {
        let mut g = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().map(|s| s.0) != Some(key) {
            *g = Some((key, load()));
            self.builds.fetch_add(1, Ordering::Relaxed);
        }
        f(&g.as_ref().expect("index just built").1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::v2;

    #[test]
    fn near_is_sorted_and_skips_far_and_missing_items() {
        let ix = BoxIndex::build(vec![
            bx_of([v2(0.0, 0.0), v2(1.0, 0.0)], 0.1),
            None,
            bx_of([v2(100.0, 0.0), v2(101.0, 0.0)], 0.1),
            bx_of([v2(-500.0, 0.0), v2(500.0, 0.0)], 0.1),
        ]);
        assert_eq!(ix.near(v2(0.5, 0.0), 0.2), vec![0, 3]);
        assert!(ix.near(v2(100.5, 3.0), 0.2).is_empty());
        assert_eq!(ix.near(v2(100.5, 0.0), 0.2), vec![2, 3]);
    }
}
