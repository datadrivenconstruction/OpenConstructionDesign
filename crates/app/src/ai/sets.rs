// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Frozen element sets: a query result keeps its ids, so a bulk edit names `s3` instead of
//! carrying 100k ids through the context, and later model changes cannot shift its pages.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Older sets go first: one long session cannot hold unbounded id lists.
const MAX_SETS: usize = ddc_ai::limits::SETS.us();
/// About 110 MB of keys and marks; the newest set stays whatever its size.
const MAX_KEYS: usize = ddc_ai::limits::SET_KEYS.us();

/// (table, id) to (category, mark) of every element of a copy: `rows::marks`.
pub type Marks = HashMap<(&'static str, i64), (&'static str, u32)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub cat: &'static str,
    pub id: i64,
}

impl Key {
    pub fn table(&self) -> &'static str {
        table_of(self.cat)
    }

    pub fn text(&self) -> String {
        format!("{}:{}", self.cat, self.id)
    }

    pub fn el(&self) -> ddc_model::ElKey {
        (self.table().to_string(), self.id)
    }
}

/// Model table of a query category.
pub fn table_of(cat: &str) -> &'static str {
    match cat {
        "door" | "window" | "opening" => "opening",
        "curtain" => "curtain_wall",
        "floor" => "slab",
        "column" => "col",
        "mep" => "mep_run",
        "imported" => "mesh",
        "wall" => "wall",
        "room" => "room",
        "beam" => "beam",
        "roof" => "roof",
        "ceiling" => "ceiling",
        "stair" => "stair",
        "railing" => "railing",
        "comp" => "comp",
        "shaft" => "shaft",
        "grid" => "grid",
        "steel_conn" => "steel_conn",
        "solid" => "solid",
        "mass" => "mass",
        _ => "",
    }
}

/// Query category of a table name or a synonym; `None` for an unknown word.
pub fn norm_cat(w: &str) -> Option<&'static str> {
    Some(match w.trim() {
        "slab" => "floor",
        "col" => "column",
        "curtain_wall" => "curtain",
        "mep_run" => "mep",
        "mesh" | "imported" => "imported",
        "shaft" | "shafts" => "shaft",
        "grid" | "grids" => "grid",
        "steel_conn" | "steel_connection" | "steel_conns" => "steel_conn",
        other => return crate::query::cat_word(other),
    })
}

#[derive(Clone, Debug)]
pub struct FrozenSet {
    pub keys: Arc<Vec<Key>>,
    pub fields: Vec<String>,
    /// Parallel to `keys` (`rows::mark_of`, 0: not known); empty when the set has no marks.
    pub marks: Arc<Vec<u32>>,
    /// The first key that came back from a new copy as another element: rowids are reused.
    pub replaced: Option<Key>,
}

#[derive(Default)]
pub struct Sets {
    next: u32,
    keys: usize,
    map: BTreeMap<u32, FrozenSet>,
}

impl Sets {
    pub fn add(&mut self, keys: Vec<Key>, fields: Vec<String>) -> u32 {
        self.add_marked(keys, fields, vec![])
    }

    pub fn add_marked(&mut self, keys: Vec<Key>, fields: Vec<String>, marks: Vec<u32>) -> u32 {
        self.next += 1;
        self.keys += keys.len();
        self.map.insert(
            self.next,
            FrozenSet {
                keys: Arc::new(keys),
                fields,
                marks: Arc::new(marks),
                replaced: None,
            },
        );
        self.trim(MAX_SETS, MAX_KEYS);
        self.next
    }

    fn trim(&mut self, max_sets: usize, max_keys: usize) {
        while self.map.len() > max_sets || (self.keys > max_keys && self.map.len() > 1) {
            let Some(&old) = self.map.keys().next() else {
                break;
            };
            if let Some(s) = self.map.remove(&old) {
                self.keys -= s.keys.len();
            }
        }
    }

    /// After a new copy of the live model. An absent key stays for `resolve` to report.
    pub fn revalidate(&mut self, now: &Marks) {
        for s in self.map.values_mut().filter(|s| s.replaced.is_none()) {
            let marks = s.marks.iter().copied().chain(std::iter::repeat(0));
            s.replaced = s
                .keys
                .iter()
                .zip(marks)
                .find(|(k, m)| {
                    now.get(&(k.table(), k.id))
                        .is_some_and(|(cat, mark)| *cat != k.cat || (*m != 0 && *mark != *m))
                })
                .map(|(k, _)| *k);
        }
    }

    /// The plan was applied: what its own items changed is not a replacement.
    pub fn remark(&mut self, now: &Marks) {
        for s in self.map.values_mut().filter(|s| !s.marks.is_empty()) {
            s.marks = Arc::new(
                s.keys
                    .iter()
                    .map(|k| now.get(&(k.table(), k.id)).map_or(0, |x| x.1))
                    .collect(),
            );
        }
    }

    pub fn get(&self, id: u32) -> Option<&FrozenSet> {
        self.map.get(&id)
    }
}

/// `s3` → 3.
pub fn parse_set(s: &str) -> Option<u32> {
    s.trim().strip_prefix('s')?.parse().ok()
}

/// `s3@50` → (3, 50).
pub fn parse_cursor(s: &str) -> Option<(u32, usize)> {
    let (a, b) = s.trim().split_once('@')?;
    Some((parse_set(a)?, b.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip() {
        assert_eq!(parse_set("s12"), Some(12));
        assert_eq!(parse_cursor("s3@50"), Some((3, 50)));
        assert_eq!(parse_cursor("s3"), None);
        assert_eq!(parse_set("door"), None);
    }

    #[test]
    fn keys_name_their_table() {
        let k = Key {
            cat: "door",
            id: 55,
        };
        assert_eq!(k.text(), "door:55");
        assert_eq!(k.el(), ("opening".to_string(), 55));
        assert_eq!(norm_cat("slab"), Some("floor"));
        assert_eq!(table_of(norm_cat(&crate::app2_fx::fx("sets_columns")).unwrap()), "col");
        assert_eq!(norm_cat(&crate::app2_fx::fx("sets_door")), Some("door"));
        assert_eq!(norm_cat("nonsense"), None);
    }

    #[test]
    fn the_key_budget_drops_old_sets_but_never_the_newest() {
        let key = |id: i64| Key { cat: "wall", id };
        let mut s = Sets::default();
        let a = s.add(vec![key(1), key(2)], vec![]);
        let b = s.add(vec![key(3), key(4)], vec![]);
        s.trim(MAX_SETS, 3);
        assert!(s.get(a).is_none());
        assert!(s.get(b).is_some());
        s.trim(MAX_SETS, 1);
        assert!(s.get(b).is_some());
        assert_eq!(s.keys, 2);
    }

    #[test]
    fn a_key_that_came_back_as_another_element_is_remembered() {
        let mut s = Sets::default();
        let same = s.add_marked(vec![Key { cat: "door", id: 1 }], vec![], vec![7]);
        let other_cat = s.add_marked(vec![Key { cat: "door", id: 2 }], vec![], vec![7]);
        let other_mark = s.add_marked(vec![Key { cat: "door", id: 3 }], vec![], vec![7]);
        let gone = s.add_marked(vec![Key { cat: "door", id: 4 }], vec![], vec![7]);
        let now: Marks = [
            (("opening", 1), ("door", 7)),
            (("opening", 2), ("window", 7)),
            (("opening", 3), ("door", 8)),
        ]
        .into_iter()
        .collect();
        s.revalidate(&now);
        assert!(s.get(same).unwrap().replaced.is_none());
        assert!(s.get(other_cat).unwrap().replaced.is_some());
        assert!(s.get(other_mark).unwrap().replaced.is_some());
        assert!(s.get(gone).unwrap().replaced.is_none());
    }

    #[test]
    fn an_applied_change_is_not_a_replacement() {
        let mut s = Sets::default();
        let id = s.add_marked(vec![Key { cat: "door", id: 1 }], vec![], vec![7]);
        let now: Marks = [(("opening", 1), ("door", 8))].into_iter().collect();
        s.remark(&now);
        s.revalidate(&now);
        assert!(s.get(id).unwrap().replaced.is_none());
    }

    #[test]
    fn old_sets_are_dropped_and_ids_never_reused() {
        let mut s = Sets::default();
        let first = s.add(vec![], vec![]);
        for _ in 0..MAX_SETS {
            s.add(vec![], vec![]);
        }
        assert!(s.get(first).is_none());
        assert_eq!(s.add(vec![], vec![]), MAX_SETS as u32 + 2);
    }
}
