//! A schedule laid out by key: category, columns, grouping, sorting and column filters. The
//! assistant makes one; the view takes it as its own, so the person edits it from there.

use super::{Editor, TabState, Units, View};
use crate::schedule::group::{self, Line};
use crate::schedule::{self, Cat, Table};

/// Column names here are column keys of the category's table.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub cat: Cat,
    /// Shown columns; empty: all.
    pub columns: Vec<String>,
    /// Group levels, outermost first.
    pub group: Vec<String>,
    /// Sort column and whether it ascends.
    pub sort: Option<(String, bool)>,
    /// Column filters in the syntax of the filter row.
    pub filters: Vec<(String, String)>,
    pub subtotals: bool,
}

/// What a preset shows on a table.
pub struct Laid {
    pub ord: Vec<u32>,
    pub cols: Vec<usize>,
    pub lines: Vec<Line>,
}

fn index(tb: &Table, key: &str) -> Option<usize> {
    tb.cols.iter().position(|c| c.key == key)
}

impl Preset {
    /// Rows, columns and group lines in the order the view shows them.
    pub fn lay(&self, tb: &Table, u: Units) -> Laid {
        let mut filters = vec![String::new(); tb.cols.len()];
        for (k, f) in &self.filters {
            if let Some(j) = index(tb, k) {
                filters[j] = f.clone();
            }
        }
        let sort = self
            .sort
            .as_ref()
            .and_then(|(k, asc)| index(tb, k).map(|j| (j, *asc)));
        let mut ord = schedule::order(tb, &filters, sort, u);
        let by: Vec<usize> = self.group.iter().filter_map(|k| index(tb, k)).collect();
        group::sort(tb, &mut ord, &by);
        let lines = group::lines(tb, &ord, &by, self.subtotals, u);
        let cols = if self.columns.is_empty() {
            (0..tb.cols.len()).collect()
        } else {
            self.columns.iter().filter_map(|k| index(tb, k)).collect()
        };
        Laid { ord, cols, lines }
    }
}

impl View {
    /// Shows `p` as this view's own state for its category: the person's filters, groups and
    /// sorting there are replaced, and every one of them can be changed again.
    pub fn apply_preset(&mut self, ed: &Editor, p: &Preset) {
        let tb = schedule::build_in(ed, p.cat, self.ru);
        let n = tb.cols.len();
        let mut t = TabState::default();
        t.filters = vec![String::new(); n];
        for (k, f) in &p.filters {
            if let Some(j) = index(&tb, k) {
                t.filters[j] = f.clone();
            }
        }
        t.hidden = tb
            .cols
            .iter()
            .map(|c| !p.columns.is_empty() && !p.columns.contains(&c.key))
            .collect();
        t.group = p.group.clone();
        t.subtotals = p.subtotals;
        t.sort = p
            .sort
            .as_ref()
            .and_then(|(k, asc)| index(&tb, k).map(|j| (j, *asc)));
        // the person's formula columns and stage filter stay with the category
        let old = std::mem::take(&mut self.tabs[p.cat.index()]);
        t.calcs = old.calcs;
        t.phase = old.phase;
        t.widths = old.widths;
        self.tabs[p.cat.index()] = t;
        self.cat = p.cat;
        self.key = None;
        self.active = None;
        self.dirty = true;
    }

    /// The group levels and sort the view holds for `cat`, by column key (for tests and the card).
    pub fn preset_of(&self, cat: Cat) -> Preset {
        let t = &self.tabs[cat.index()];
        let tb = self.table.as_ref().filter(|tb| tb.cat == cat);
        let key = |j: usize| tb.and_then(|tb| tb.cols.get(j)).map(|c| c.key.clone());
        Preset {
            cat,
            columns: vec![],
            group: t.group.clone(),
            sort: t.sort.and_then(|(j, asc)| key(j).map(|k| (k, asc))),
            filters: t
                .filters
                .iter()
                .enumerate()
                .filter(|(_, f)| !f.trim().is_empty())
                .filter_map(|(j, f)| key(j).map(|k| (k, f.clone())))
                .collect(),
            subtotals: t.subtotals,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walls() -> (Editor, usize) {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut ed);
        let n = ed.walls.len();
        assert!(n >= 2, "the fixture room has walls");
        (ed, n)
    }

    #[test]
    fn a_preset_becomes_the_views_own_state() {
        let (ed, n) = walls();
        let tb = schedule::build_in(&ed, Cat::Walls, false);
        let ty = tb.cols.iter().find(|c| c.key == "type").map(|c| c.key.clone());
        let ty = ty.expect("walls have a type column");
        let p = Preset {
            cat: Cat::Walls,
            columns: vec!["id".into(), ty.clone()],
            group: vec![ty.clone()],
            sort: Some(("id".into(), false)),
            filters: vec![],
            subtotals: true,
        };
        let mut v = View::default();
        v.ru = false;
        v.apply_preset(&ed, &p);
        v.sync(&ed);
        assert_eq!(v.cat, Cat::Walls);
        assert_eq!(v.ord.len(), n);
        assert_eq!(v.visible_cols().len(), 2);
        assert!(!v.group_lines().is_empty(), "grouped");
        let back = v.preset_of(Cat::Walls);
        assert_eq!(back.group, vec![ty]);
        assert_eq!(back.sort, Some(("id".to_string(), false)));
        assert!(back.subtotals);
        // the view is still the person's: its own sort can change afterwards
        v.tabs[Cat::Walls.index()].sort = None;
        assert_eq!(v.preset_of(Cat::Walls).sort, None);
    }

    #[test]
    fn lay_filters_sorts_and_groups_like_the_view() {
        let (ed, n) = walls();
        let tb = schedule::build_in(&ed, Cat::Walls, false);
        let p = Preset {
            cat: Cat::Walls,
            columns: vec![],
            group: vec![],
            sort: Some(("id".into(), false)),
            filters: vec![],
            subtotals: false,
        };
        let l = p.lay(&tb, ed.tool.units);
        assert_eq!(l.ord.len(), n);
        let ids: Vec<i64> = l.ord.iter().map(|i| tb.rows[*i as usize].id).collect();
        assert!(ids.windows(2).all(|w| w[0] >= w[1]), "descending: {ids:?}");
        assert!(l.lines.is_empty());
        assert_eq!(l.cols.len(), tb.cols.len());
        // a filter that matches nothing leaves nothing
        let none = Preset { filters: vec![("id".into(), "=-5".into())], ..p };
        assert!(none.lay(&tb, ed.tool.units).ord.is_empty());
    }
}
