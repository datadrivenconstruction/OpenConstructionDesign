// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The whole selection for the 3D view: scene element ranges of every selected item, merged,
//! and rebuilt only when the selection or the model changes (a large selection must not cost
//! a lookup per item every frame).

use crate::editor::{Editor, Sel};
use std::sync::Arc;

/// Merged ranges of `sel`; items `range` does not know (not in the scene) are left out.
pub fn ranges_of(sel: &[Sel], range: impl Fn(Sel) -> Option<(u32, u32)>) -> Vec<(u32, u32)> {
    ddc_render::selset::merge_ranges(sel.iter().filter_map(|s| range(*s)).collect())
}

/// The single range the uniforms carry: a one-range set is the whole selection (it may merge
/// the first item with its neighbours); two or more go to the selection texture.
pub fn shader_range(first: Option<(u32, u32)>, set: &[(u32, u32)]) -> Option<(u32, u32)> {
    match set {
        [one] => Some(*one),
        _ => first,
    }
}

/// Ranges of the last selection and the model revision they were built for.
pub struct SelCache {
    key: Option<(u64, Vec<Sel>)>,
    ranges: Arc<[(u32, u32)]>,
}

impl Default for SelCache {
    fn default() -> Self {
        Self {
            key: None,
            ranges: Arc::from(Vec::new()),
        }
    }
}

impl SelCache {
    /// The cached ranges are stale for this revision and selection.
    pub fn stale(&self, revision: u64, sel: &[Sel]) -> bool {
        self.key
            .as_ref()
            .is_none_or(|(r, s)| *r != revision || s.as_slice() != sel)
    }

    /// Ranges of the editor's selection; the same `Arc` while nothing changed.
    pub fn ranges(&mut self, ed: &Editor) -> Arc<[(u32, u32)]> {
        if ed.selection.is_empty() {
            // a picked solid lives outside the selection list: cheap, never cached
            self.key = None;
            let r = ed.highlight_all();
            if self.ranges[..] != r[..] {
                self.ranges = Arc::from(r);
            }
            return self.ranges.clone();
        }
        if self.stale(ed.revision, &ed.selection) {
            self.ranges = Arc::from(ed.highlight_all());
            self.key = Some((ed.revision, ed.selection.clone()));
        }
        self.ranges.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(s: Sel) -> Option<(u32, u32)> {
        match s {
            Sel::Wall(id) if id >= 0 => Some((id as u32, id as u32 + 1)),
            // curtain wall: mullions and panels as one block
            Sel::Opening(id) => Some((100 + id as u32 * 10, 110 + id as u32 * 10)),
            _ => None,
        }
    }

    #[test]
    fn ranges_empty_one_many_merged() {
        assert!(ranges_of(&[], range).is_empty());
        assert_eq!(ranges_of(&[Sel::Wall(4)], range), vec![(4, 5)]);
        // contiguous walls fold into one range, order of picking does not matter
        assert_eq!(
            ranges_of(&[Sel::Wall(3), Sel::Wall(1), Sel::Wall(2), Sel::Wall(7)], range),
            vec![(1, 4), (7, 8)]
        );
        assert_eq!(
            ranges_of(&[Sel::Opening(1), Sel::Wall(0), Sel::Opening(0)], range),
            vec![(0, 1), (100, 120)]
        );
    }

    #[test]
    fn shader_range_prefers_a_one_range_set() {
        assert_eq!(shader_range(None, &[]), None);
        assert_eq!(shader_range(Some((3, 4)), &[]), Some((3, 4)));
        // first wall merged with its neighbour: the whole block is lit
        assert_eq!(shader_range(Some((0, 1)), &[(0, 2)]), Some((0, 2)));
        // first item outside the scene, the rest in one block
        assert_eq!(shader_range(None, &[(5, 6)]), Some((5, 6)));
        assert_eq!(shader_range(Some((0, 1)), &[(0, 1), (4, 5)]), Some((0, 1)));
    }

    #[test]
    fn unknown_items_are_ignored() {
        assert_eq!(
            ranges_of(&[Sel::Wall(-5), Sel::Wall(2), Sel::Wall(-1)], range),
            vec![(2, 3)]
        );
    }

    #[test]
    fn stale_on_selection_or_revision_change() {
        let mut c = SelCache::default();
        let sel = vec![Sel::Wall(1), Sel::Wall(2)];
        assert!(c.stale(1, &sel));
        c.key = Some((1, sel.clone()));
        assert!(!c.stale(1, &sel));
        assert!(c.stale(2, &sel), "model changed");
        assert!(c.stale(1, &sel[..1]), "one fewer");
        assert!(c.stale(1, &[Sel::Wall(2), Sel::Wall(1)]), "other order is a new list");
    }

    #[test]
    fn large_selection_builds_once() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut c = SelCache::default();
        let first = c.ranges(&ed);
        assert!(first.is_empty());
        // ten thousand ids the scene does not know: built once, then the same Arc
        ed.selection = (0..10_000).map(Sel::Wall).collect();
        let a = c.ranges(&ed);
        let b = c.ranges(&ed);
        assert!(Arc::ptr_eq(&a, &b));
        assert!(a.is_empty());
        ed.selection.clear();
        assert!(c.ranges(&ed).is_empty());
    }
}
