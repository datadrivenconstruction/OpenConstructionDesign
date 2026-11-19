// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan paint helpers: O(1) selection membership and world bounding boxes for screen culling.

use crate::editor::{Editor, Sel};
use ddc_geom::V2;
use std::collections::HashSet;

/// Hash index over the ordered selection list; the list keeps order for the UI.
#[derive(Default, Clone, Debug)]
pub struct SelIndex(HashSet<Sel>);

impl SelIndex {
    pub fn new(list: &[Sel]) -> Self {
        Self(list.iter().copied().collect())
    }
    pub fn contains(&self, s: &Sel) -> bool {
        self.0.contains(s)
    }
    /// True when the index holds exactly the members of `list`.
    pub fn matches(&self, list: &[Sel]) -> bool {
        list.iter().all(|s| self.0.contains(s)) && self.0.len() == list.iter().collect::<HashSet<_>>().len()
    }
}

/// World box: min x, min y, max x, max y.
pub type Bb = [f64; 4];

pub fn rings_bb<'a>(rings: impl IntoIterator<Item = &'a Vec<V2>>) -> Option<Bb> {
    let mut bb: Option<Bb> = None;
    for p in rings.into_iter().flatten() {
        let b = bb.get_or_insert([p.x, p.y, p.x, p.y]);
        b[0] = b[0].min(p.x);
        b[1] = b[1].min(p.y);
        b[2] = b[2].max(p.x);
        b[3] = b[3].max(p.y);
    }
    bb
}

/// World view box from two opposite corners, grown by `margin`.
pub fn view_bb(a: V2, b: V2, margin: f64) -> Bb {
    [
        a.x.min(b.x) - margin,
        a.y.min(b.y) - margin,
        a.x.max(b.x) + margin,
        a.y.max(b.y) + margin,
    ]
}

/// An item without a box is always drawn.
pub fn hits(item: Option<Bb>, view: Bb) -> bool {
    item.is_none_or(|b| b[0] <= view[2] && b[2] >= view[0] && b[1] <= view[3] && b[3] >= view[1])
}

pub type CullKey = (u64, u64, usize, usize, usize, u64);

/// Boxes parallel to `ed.plan` (walls) and `ed.slabs`, rebuilt only when the key changes.
#[derive(Default)]
pub struct CullCache {
    key: Option<CullKey>,
    pub walls: Vec<Option<Bb>>,
    pub slabs: Vec<Option<Bb>>,
}

impl CullCache {
    pub fn refresh(&mut self, key: CullKey, ed: &Editor) {
        if self.key == Some(key) && self.walls.len() == ed.plan.len() && self.slabs.len() == ed.slabs.len() {
            return;
        }
        self.walls = ed.plan.iter().map(rings_bb).collect();
        self.slabs = ed
            .slabs
            .iter()
            .map(|s| rings_bb(s.rings.iter().chain(s.cuts.iter())))
            .collect();
        self.key = Some(key);
    }
    pub fn wall(&self, i: usize) -> Option<Bb> {
        self.walls.get(i).copied().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::v2;

    fn square(x: f64, y: f64) -> Vec<V2> {
        vec![v2(x, y), v2(x + 1.0, y), v2(x + 1.0, y + 1.0), v2(x, y + 1.0)]
    }

    #[test]
    fn selection_index_matches_list_after_edits() {
        let mut list: Vec<Sel> = (0..10).map(Sel::Wall).collect();
        let mut idx = SelIndex::new(&list);
        assert!(idx.matches(&list));
        list.retain(|s| *s != Sel::Wall(3));
        assert!(!idx.matches(&list));
        idx = SelIndex::new(&list);
        assert!(idx.matches(&list) && !idx.contains(&Sel::Wall(3)) && idx.contains(&Sel::Wall(4)));
        list.push(Sel::Room(7));
        idx = SelIndex::new(&list);
        assert!(idx.contains(&Sel::Room(7)));
        list.clear();
        idx = SelIndex::new(&list);
        assert!(idx.matches(&list) && !idx.contains(&Sel::Wall(4)));
    }

    #[test]
    fn culled_items_not_painted() {
        let rings: Vec<Vec<Vec<V2>>> = (0..100)
            .map(|i| vec![square((i % 10) as f64 * 3.0, (i / 10) as f64 * 3.0)])
            .collect();
        let bbs: Vec<Option<Bb>> = rings.iter().map(|r| rings_bb(r)).collect();
        let view = view_bb(v2(4.0, 4.0), v2(10.0, 10.0), 0.1);
        let kept: Vec<usize> = (0..100).filter(|&i| hits(bbs[i], view)).collect();
        // reference: any vertex inside the view or the view inside the item box
        for i in 0..100 {
            let b = bbs[i].unwrap();
            let overlap = b[0] <= view[2] && b[2] >= view[0] && b[1] <= view[3] && b[3] >= view[1];
            assert_eq!(kept.contains(&i), overlap, "item {i}");
        }
        assert!(!kept.is_empty() && kept.len() < 100);
        assert!(!kept.contains(&0) && !kept.contains(&99));
        assert!(hits(None, view));
    }

    /// Points along every edge of the rings (the strokes the unculled plan would paint).
    fn edge_samples(rings: &[Vec<V2>]) -> Vec<V2> {
        let mut out = vec![];
        for r in rings {
            for k in 0..r.len() {
                let (a, b) = (r[k], r[(k + 1) % r.len()]);
                for t in 0..=16 {
                    let f = t as f64 / 16.0;
                    out.push(v2(a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f));
                }
            }
        }
        out
    }

    #[test]
    fn culled_plan_keeps_every_visible_primitive() {
        // skewed quads of mixed size; long thin ones cross the view without a vertex inside it
        let mut st = 17u64;
        let mut rnd = move || {
            st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (st >> 33) as f64 / (1u64 << 31) as f64
        };
        let items: Vec<Vec<Vec<V2>>> = (0..400)
            .map(|i| {
                let (x, y) = (rnd() * 60.0 - 10.0, rnd() * 60.0 - 10.0);
                let (w, h) = if i % 7 == 0 { (80.0, 0.3) } else { (rnd() * 3.0, rnd() * 3.0) };
                vec![vec![v2(x, y), v2(x + w, y + 0.2), v2(x + w, y + h), v2(x - 0.1, y + h)]]
            })
            .collect();
        let bbs: Vec<Option<Bb>> = items.iter().map(|r| rings_bb(r)).collect();
        for (a, b) in [(v2(5.0, 5.0), v2(15.0, 12.0)), (v2(-3.0, 20.0), v2(40.0, 21.0))] {
            let view = view_bb(a, b, 0.0);
            let inside = |p: &V2| p.x >= view[0] && p.x <= view[2] && p.y >= view[1] && p.y <= view[3];
            // unculled: everything with a painted point in the view; culled must draw the same
            let unculled: Vec<usize> =
                (0..items.len()).filter(|&i| edge_samples(&items[i]).iter().any(inside)).collect();
            let culled: Vec<usize> = (0..items.len()).filter(|&i| hits(bbs[i], view)).collect();
            assert!(!unculled.is_empty(), "view sees nothing");
            for i in &unculled {
                assert!(culled.contains(i), "visible item {i} was culled");
            }
            // what culling skipped paints nothing in the view
            for i in (0..items.len()).filter(|i| !culled.contains(i)) {
                assert!(!edge_samples(&items[i]).iter().any(inside), "item {i}");
            }
            assert!(culled.len() < items.len());
        }
    }

    #[test]
    fn plan_paint_20k_walls_zoomed_budget() {
        let bbs: Vec<Option<Bb>> = (0..20_000)
            .map(|i| rings_bb(&vec![square((i % 200) as f64 * 3.0, (i / 200) as f64 * 3.0)]))
            .collect();
        let view = view_bb(v2(0.0, 0.0), v2(20.0, 20.0), 0.2);
        let t = web_time::Instant::now();
        let n = bbs.iter().filter(|b| hits(**b, view)).count();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        println!("cull 20k: {n} kept, {ms:.2} ms");
        assert!(n > 0 && n < 100);
        assert!(ms < 200.0);
    }

    #[test]
    fn select_all_10k_walls_frame_budget() {
        let list: Vec<Sel> = (0..10_000).map(Sel::Wall).collect();
        let t = web_time::Instant::now();
        let idx = SelIndex::new(&list[..5_000]);
        let hit = (0..10_000).filter(|&i| idx.contains(&Sel::Wall(i))).count();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        println!("selset 10k walls / 5k selected: {ms:.2} ms");
        assert_eq!(hit, 5_000);
        assert!(ms < 500.0);
        // the old per-element scan, for scale: must not be needed
        assert!(idx.matches(&list[..5_000]));
    }
}
