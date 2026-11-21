// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Construction order for the "as built" reveal: edits (added, deleted, changed) are arranged
//! into batches: foundations, columns, slabs, walls, beams and trusses, stairs, roof, openings,
//! networks, components; within a batch by level elevation, then by bottom, then by number.
//! Deletions come before additions and in reverse order (top to bottom). A pure function
//! of the steps and the model; no meshes are built.

use std::cmp::Ordering;

use rusqlite::OptionalExtension;

use crate::{ElKey, Model};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Change {
    Added,
    Removed,
    Modified,
}

/// One element edit. `from` is for an element moved with no other change: the offset of the old
/// position relative to the new one (old − new, m); the reveal timeline turns it into motion.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub key: ElKey,
    pub change: Change,
    pub from: Option<[f64; 3]>,
}

impl Step {
    pub fn new(key: ElKey, change: Change) -> Self {
        Self { key, change, from: None }
    }
}

/// A construction batch; the value order is the reveal order for additions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Footing,
    Column,
    Slab,
    Wall,
    Frame,
    Stair,
    Roof,
    Opening,
    Network,
    Component,
    Other,
}

/// Batch of an element's table; unknown tables go last.
pub fn stage_of(table: &str) -> Stage {
    match table {
        "iso_footing" | "strip_footing" | "raft" => Stage::Footing,
        "col" => Stage::Column,
        "slab" | "ceiling" => Stage::Slab,
        "wall" | "curtain_wall" => Stage::Wall,
        "beam" | "brace" | "truss" => Stage::Frame,
        "stair" | "railing" => Stage::Stair,
        "roof" => Stage::Roof,
        "opening" | "shaft" => Stage::Opening,
        t if t.starts_with("mep_") => Stage::Network,
        "comp" => Stage::Component,
        _ => Stage::Other,
    }
}

/// Position columns: moving a whole element changes all of them at once and nothing else.
const PLACE: &[&str] = &["ax", "ay", "bx", "by", "x", "y", "az", "bz", "z", "base_offset"];

fn num(s: &Option<&str>) -> Option<f64> {
    // num-lint: a stored order value, invariant form
    s.as_ref()?.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Element offset from changed values (column, was, now): all changes are position columns, and
/// the endpoints (`a*`, `b*`) are shifted by the same amount. Otherwise (stretched, rotated, any
/// other edit) `None`. The result is was − now.
pub fn shift_of(cells: &[(&str, Option<&str>, Option<&str>)]) -> Option<[f64; 3]> {
    if cells.is_empty() || cells.iter().any(|c| !PLACE.contains(&c.0)) {
        return None;
    }
    let d = |col: &str| -> Option<Option<f64>> {
        match cells.iter().find(|c| c.0 == col) {
            None => Some(None),
            Some(c) => Some(Some(num(&c.1)? - num(&c.2)?)),
        }
    };
    let pair = |single: &[&str], a: &str, b: &str| -> Option<f64> {
        let mut found = vec![];
        for s in single {
            if let Some(v) = d(s)? {
                found.push(v);
            }
        }
        match (d(a)?, d(b)?) {
            (Some(x), Some(y)) => {
                if (x - y).abs() > 1e-9 {
                    return None;
                }
                found.push(x);
            }
            (None, None) => {}
            // stretched by one endpoint
            _ => return None,
        }
        match found.as_slice() {
            [] => Some(0.0),
            [v] => Some(*v),
            _ => None,
        }
    };
    let dx = pair(&["x"], "ax", "bx")?;
    let dy = pair(&["y"], "ay", "by")?;
    let dz = pair(&["z", "base_offset"], "az", "bz")?;
    (dx.abs() > 1e-9 || dy.abs() > 1e-9 || dz.abs() > 1e-9).then_some([dx, dy, dz])
}

/// Level elevation and element bottom, m; (0, 0) if the row or table is missing.
fn place(m: &Model, table: &str, id: i64) -> (f64, f64) {
    // the table name is interpolated into the query only from this list
    let (level, z) = match table {
        "wall" | "stair" | "col" => ("t.level", "COALESCE(t.base_offset, 0)"),
        "beam" | "brace" | "truss" => ("t.level", "MIN(t.az, t.bz)"),
        "comp" => ("t.level", "t.z"),
        "iso_footing" | "strip_footing" | "raft" | "slab" | "ceiling" | "curtain_wall" | "roof"
        | "railing" | "shaft" | "mep_run" => ("t.level", "0"),
        "opening" => ("(SELECT level FROM wall WHERE id = t.wall)", "0"),
        _ => return (0.0, 0.0),
    };
    let sql = format!(
        "SELECT COALESCE((SELECT elevation FROM level WHERE id = {level}), 0), {z} FROM {table} t WHERE t.id = ?1"
    );
    m.sql()
        .query_row(&sql, [id], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)))
        .optional()
        .ok()
        .flatten()
        .unwrap_or((0.0, 0.0))
}

fn cmp_key(a: &(Stage, f64, f64, &ElKey), b: &(Stage, f64, f64, &ElKey)) -> Ordering {
    a.0.cmp(&b.0)
        .then(a.1.total_cmp(&b.1))
        .then(a.2.total_cmp(&b.2))
        .then_with(|| a.3.cmp(b.3))
}

/// Construction batches: deletions first (batches in reverse order, top to bottom within), then
/// additions and changes in construction order (bottom to top). Empty batches are omitted; steps
/// with equal keys keep their order. A deleted element is no longer in the model, so its position
/// comes from the table if the row remains, otherwise (0, 0).
pub fn build_order(steps: &[Step], m: &Model) -> Vec<Vec<Step>> {
    let keyed = |s: &Step| {
        let (lv, z) = place(m, &s.key.0, s.key.1);
        (stage_of(&s.key.0), lv, z)
    };
    let mut out: Vec<Vec<Step>> = vec![];
    // deletions: reverse order of batches and elevations
    let mut rem: Vec<(&Step, (Stage, f64, f64))> = steps
        .iter()
        .filter(|s| s.change == Change::Removed)
        .map(|s| (s, keyed(s)))
        .collect();
    rem.sort_by(|a, b| {
        cmp_key(&(a.1 .0, a.1 .1, a.1 .2, &a.0.key), &(b.1 .0, b.1 .1, b.1 .2, &b.0.key)).reverse()
    });
    let mut add: Vec<(&Step, (Stage, f64, f64))> = steps
        .iter()
        .filter(|s| s.change != Change::Removed)
        .map(|s| (s, keyed(s)))
        .collect();
    add.sort_by(|a, b| {
        cmp_key(&(a.1 .0, a.1 .1, a.1 .2, &a.0.key), &(b.1 .0, b.1 .1, b.1 .2, &b.0.key))
    });
    for list in [rem, add] {
        let mut last: Option<Stage> = None;
        for (s, k) in list {
            if last != Some(k.0) {
                out.push(vec![]);
                last = Some(k.0);
            }
            out.last_mut().expect("batch just pushed").push(s.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(t: &str, id: i64, c: Change) -> Step {
        Step::new((t.to_string(), id), c)
    }

    /// A model with levels at 0 and 3 m and one element per table.
    fn model() -> (Model, [i64; 2]) {
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let l1 = m.add_level("L1", 3.0).unwrap();
        (m, [l0, l1])
    }

    fn tables(b: &[Vec<Step>]) -> Vec<Vec<(String, i64)>> {
        b.iter().map(|v| v.iter().map(|s| s.key.clone()).collect()).collect()
    }

    #[test]
    fn order_footings_before_walls_before_roof() {
        let (m, _) = model();
        // shuffled input
        let steps = [
            step("roof", 1, Change::Added),
            step("wall", 1, Change::Added),
            step("comp", 1, Change::Added),
            step("iso_footing", 1, Change::Added),
            step("slab", 1, Change::Added),
            step("col", 1, Change::Added),
            step("opening", 1, Change::Added),
            step("mep_run", 1, Change::Added),
            step("beam", 1, Change::Added),
            step("stair", 1, Change::Added),
        ];
        let b = build_order(&steps, &m);
        let names: Vec<&str> = b.iter().map(|v| v[0].key.0.as_str()).collect();
        assert_eq!(
            names,
            ["iso_footing", "col", "slab", "wall", "beam", "stair", "roof", "opening", "mep_run", "comp"]
        );
    }

    #[test]
    fn removals_go_first_and_in_reverse() {
        let (m, _) = model();
        let steps = [
            step("wall", 2, Change::Added),
            step("col", 1, Change::Removed),
            step("roof", 1, Change::Removed),
            step("iso_footing", 1, Change::Removed),
        ];
        let b = build_order(&steps, &m);
        let t = tables(&b);
        assert_eq!(
            t,
            vec![
                vec![("roof".to_string(), 1)],
                vec![("col".to_string(), 1)],
                vec![("iso_footing".to_string(), 1)],
                vec![("wall".to_string(), 2)],
            ]
        );
        assert!(b[..3].iter().flatten().all(|s| s.change == Change::Removed));
    }

    #[test]
    fn within_a_stage_by_level_then_bottom_then_id() {
        let (m, [l0, l1]) = model();
        m.sql().execute("INSERT INTO wall_type(id, name) VALUES (1, 'T')", []).unwrap();
        let wt = 1i64;
        let w = |id: i64, level: i64, base: f64| {
            m.sql()
                .execute(
                    "INSERT INTO wall(id, level, wtype, ax, ay, bx, by, base_offset, height) VALUES (?1, ?2, ?3, 0, 0, 1, 0, ?4, 3)",
                    rusqlite::params![id, level, wt, base],
                )
                .unwrap();
        };
        // level 1 comes earlier by number but is higher; on one level: by bottom, then by number
        w(1, l1, 0.0);
        w(2, l0, 0.5);
        w(3, l0, 0.0);
        w(4, l0, 0.0);
        let steps: Vec<Step> = [1, 2, 3, 4].map(|i| step("wall", i, Change::Added)).to_vec();
        let ids: Vec<i64> = build_order(&steps, &m)[0].iter().map(|s| s.key.1).collect();
        assert_eq!(ids, [3, 4, 2, 1]);
        // removals: the same walls, top first
        let rem: Vec<Step> = [1, 2, 3, 4].map(|i| step("wall", i, Change::Removed)).to_vec();
        let ids: Vec<i64> = build_order(&rem, &m)[0].iter().map(|s| s.key.1).collect();
        assert_eq!(ids, [1, 2, 4, 3]);
    }

    #[test]
    fn order_is_stable_for_equal_keys() {
        let (m, _) = model();
        // one batch, no rows in the table: equal positions are decided by the key, a repeat adds no differences
        let steps: Vec<Step> = [5, 3, 9, 1].map(|i| step("wall", i, Change::Added)).to_vec();
        let a = build_order(&steps, &m);
        let mut rev = steps.clone();
        rev.reverse();
        assert_eq!(a, build_order(&rev, &m), "input in any order gives one reveal order");
        let ids: Vec<i64> = a[0].iter().map(|s| s.key.1).collect();
        assert_eq!(ids, [1, 3, 5, 9]);
        assert_eq!(build_order(&[], &m), Vec::<Vec<Step>>::new());
    }

    #[test]
    fn shift_of_whole_wall_move_only() {
        let c = |a: &'static str, o: &'static str, n: &'static str| (a, Some(o), Some(n));
        // wall moved by (+2, 0): was − now = −2
        let moved = [c("ax", "0", "2"), c("bx", "5", "7")];
        assert_eq!(shift_of(&moved), Some([-2.0, 0.0, 0.0]));
        let diag = [c("ax", "0", "1"), c("bx", "5", "6"), c("ay", "0", "-1"), c("by", "0", "-1")];
        assert_eq!(shift_of(&diag), Some([-1.0, 1.0, 0.0]));
        // stretched: one endpoint, or endpoints by different amounts
        assert_eq!(shift_of(&[c("bx", "5", "7")]), None);
        assert_eq!(shift_of(&[c("ax", "0", "1"), c("bx", "5", "7")]), None);
        // another edit together with a move
        assert_eq!(shift_of(&[c("ax", "0", "1"), c("bx", "5", "6"), c("height", "3", "4")]), None);
        assert_eq!(shift_of(&[]), None);
        // column and component: x, y, z
        assert_eq!(shift_of(&[c("x", "1", "3"), c("y", "1", "1")]), Some([-2.0, 0.0, 0.0]));
        assert_eq!(shift_of(&[c("z", "0.5", "0")]), Some([0.0, 0.0, 0.5]));
        // non-number
        assert_eq!(shift_of(&[("x", Some("a"), Some("1"))]), None);
        assert_eq!(shift_of(&[("x", None, Some("1"))]), None);
    }
}
