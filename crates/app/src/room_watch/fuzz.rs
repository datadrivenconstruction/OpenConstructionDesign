// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Randomised wall and separator edits against the room note: every room an edit opens is named,
//! no unchanged room is named, undo and redo leave no stale note. A failing seed is printed.

use crate::editor::{Editor, OpFn};
use ddc_model::{Model, RoomSpec, SepSpec, WallSpec};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    /// Multiple of 0.25 in [-3, 3], never 0.
    fn shift(&mut self) -> f64 {
        let k = self.below(12) as f64 + 1.0;
        let v = k * 0.25;
        if self.below(2) == 0 {
            v
        } else {
            -v
        }
    }
}

/// Three rooms in a row per level: two bounded by walls, the last closed on the right by a
/// room separator. Returns room number by id.
fn fixture(e: &mut Editor) -> BTreeMap<i64, String> {
    let wt = e.types[e.current_type].id;
    let upper = e.model.add_level("Upper", 3.5).unwrap();
    let mut nums = BTreeMap::new();
    for (li, level) in [e.level, upper].into_iter().enumerate() {
        let mut specs = vec![];
        let mut wall = |a: (f64, f64), b: (f64, f64)| {
            specs.push(WallSpec {
                a,
                b,
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
        };
        for i in 0..3 {
            let x = i as f64 * 4.0;
            wall((x, 0.0), (x + 4.0, 0.0));
            wall((x + 4.0, 4.0), (x, 4.0));
        }
        for x in [0.0, 4.0, 8.0] {
            wall((x, 0.0), (x, 4.0));
        }
        e.model.add_walls(&specs).unwrap();
        e.model
            .add_room_sep(&SepSpec {
                level,
                a: (12.0, 0.0),
                b: (12.0, 4.0),
            })
            .unwrap();
        for i in 0..3 {
            let id = e
                .model
                .add_room(&RoomSpec {
                    level,
                    name: "R".into(),
                    number: format!("Q{}", 100 + li * 3 + i),
                    point: (i as f64 * 4.0 + 2.0, 2.0),
                    height: 3.0,
                })
                .unwrap();
            nums.insert(id, format!("Q{}", 100 + li * 3 + i));
        }
    }
    e.reload().unwrap();
    nums
}

/// Rooms without a measured area, the watch's own test for "not enclosed"; the watch must flag
/// exactly these.
fn open_rooms(e: &Editor) -> BTreeSet<i64> {
    let open: BTreeSet<i64> = e
        .model
        .sql()
        .prepare(
            "SELECT r.id FROM room r LEFT JOIN room_qty q ON q.room=r.id \
             WHERE COALESCE(q.area, 0) <= 0",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let flagged: BTreeSet<i64> = e.room_watch.flagged().map(|(id, _)| id).collect();
    assert_eq!(flagged, open, "the watch flags exactly the rooms without area");
    open
}

/// Room ids whose number appears in the status line. The duplicate-region note also cites the
/// room that keeps the quantities; that citation is a reference, so only its subject counts.
fn named(e: &Editor, nums: &BTreeMap<i64, String>) -> BTreeSet<i64> {
    let mut s = e.status.clone().unwrap_or_default();
    for (room, other) in crate::room_watch::snapshot(&e.model) {
        let room = crate::room_watch::label(&e.model, room);
        let other = crate::room_watch::label(&e.model, other);
        let dup = crate::tr("room_watch.room.dup", &[("room", &room), ("other", &other)]);
        s = s.replace(&dup, &room);
    }
    let words: BTreeSet<&str> = s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    nums.iter()
        .filter(|(_, n)| words.contains(n.as_str()))
        .map(|(id, _)| *id)
        .collect()
}

#[derive(Clone, Copy)]
enum Check {
    Opened,
    NoFalse,
    Undo,
}

/// Compares one step: `lost` rooms must be named, nothing outside the changed set is named.
fn compare(
    e: &Editor,
    nums: &BTreeMap<i64, String>,
    before: &BTreeSet<i64>,
    what: &str,
    seed: u64,
    which: Check,
) -> BTreeSet<i64> {
    let after = open_rooms(e);
    let names = named(e, nums);
    let lost: BTreeSet<i64> = after.difference(before).copied().collect();
    let changed: BTreeSet<i64> = after.symmetric_difference(before).copied().collect();
    if matches!(which, Check::Opened | Check::Undo) {
        assert!(
            lost.is_subset(&names),
            "seed {seed} {what}: opened {lost:?} but status names {names:?}: {:?}",
            e.status
        );
    }
    if matches!(which, Check::NoFalse | Check::Undo) {
        assert!(
            names.is_subset(&changed),
            "seed {seed} {what}: status names {names:?}, changed only {changed:?}: {:?}",
            e.status
        );
    }
    after
}

/// A random edit as a model operation; `None` when nothing applies.
fn random_op(e: &Editor, rng: &mut Rng) -> Option<(String, OpFn)> {
    let level = {
        let mut st = e.model.sql().prepare("SELECT id FROM level ORDER BY id").ok()?;
        let ls: Vec<i64> = st
            .query_map([], |r| r.get(0))
            .ok()?
            .collect::<Result<_, _>>()
            .ok()?;
        *ls.get(rng.below(ls.len().max(1)))?
    };
    let walls: Vec<(i64, [f64; 4])> = {
        let mut st = e
            .model
            .sql()
            .prepare("SELECT id, ax, ay, bx, by FROM wall WHERE level=?1 ORDER BY id")
            .ok()?;
        let rows = st
            .query_map([level], |r| {
                Ok((
                    r.get(0)?,
                    [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?],
                ))
            })
            .ok()?;
        rows.collect::<Result<_, _>>().ok()?
    };
    let seps: Vec<i64> = {
        let mut st = e
            .model
            .sql()
            .prepare("SELECT id FROM room_sep WHERE level=?1 ORDER BY id")
            .ok()?;
        let rows = st.query_map([level], |r| r.get(0)).ok()?;
        rows.collect::<Result<_, _>>().ok()?
    };
    let kind = rng.below(6);
    if kind == 5 {
        let id = *seps.get(rng.below(seps.len().max(1)))?;
        return Some((
            format!("delete sep {id}"),
            OpFn(Arc::new(move |m: &mut Model| m.delete_room_sep(id))),
        ));
    }
    if walls.is_empty() {
        return None;
    }
    let (id, [ax, ay, bx, by]) = walls[rng.below(walls.len())];
    Some(match kind {
        0 => {
            let (dx, dy) = (rng.shift(), rng.shift());
            (
                format!("move wall {id} by ({dx}, {dy})"),
                OpFn(Arc::new(move |m: &mut Model| {
                    m.set_wall_axis(id, (ax + dx, ay + dy), (bx + dx, by + dy))
                })),
            )
        }
        1 => (
            format!("delete wall {id}"),
            OpFn(Arc::new(move |m: &mut Model| m.delete_wall(id))),
        ),
        2 => {
            let ang = (rng.below(11) as f64 + 1.0) * 15.0_f64.to_radians();
            let (mx, my) = ((ax + bx) / 2.0, (ay + by) / 2.0);
            let (hx, hy) = ((bx - ax) / 2.0, (by - ay) / 2.0);
            let (c, s) = (ang.cos(), ang.sin());
            let (rx, ry) = (hx * c - hy * s, hx * s + hy * c);
            (
                format!("rotate wall {id} by {} deg", ang.to_degrees().round()),
                OpFn(Arc::new(move |m: &mut Model| {
                    m.set_wall_axis(id, (mx - rx, my - ry), (mx + rx, my + ry))
                })),
            )
        }
        3 => {
            let t = ((bx - ax).hypot(by - ay)) * (0.25 + 0.25 * rng.below(3) as f64);
            (
                format!("split wall {id} at {t}"),
                OpFn(Arc::new(move |m: &mut Model| m.split_wall(id, t).map(|_| ()))),
            )
        }
        _ => {
            // move one end only
            let (dx, dy) = (rng.shift(), rng.shift());
            (
                format!("move end of wall {id} by ({dx}, {dy})"),
                OpFn(Arc::new(move |m: &mut Model| {
                    m.set_wall_axis(id, (ax, ay), (bx + dx, by + dy))
                })),
            )
        }
    })
}

fn run(seed: u64, steps: usize, which: Check) {
    let mut e = Editor::new_in_memory().unwrap();
    let nums = fixture(&mut e);
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut open = open_rooms(&e);
    assert!(open.is_empty(), "seed {seed}: fixture rooms not enclosed: {open:?}");
    for step in 0..steps {
        let Some((what, op)) = random_op(&e, &mut rng) else {
            continue;
        };
        let what = format!("step {step} {what}");
        let before = open.clone();
        e.status = None;
        if e.run_op(op).is_err() {
            // a refused edit leaves the model as it was
            assert_eq!(open_rooms(&e), before, "seed {seed} {what}: refused op changed rooms");
            continue;
        }
        let after = compare(&e, &nums, &before, &what, seed, which);
        if matches!(which, Check::Undo) {
            e.status = None;
            e.undo();
            // the frame's end: undo sets its own status after the reload
            e.settle_status();
            let undone = compare(&e, &nums, &after, &format!("{what} / undo"), seed, which);
            assert_eq!(undone, before, "seed {seed} {what}: undo did not restore rooms");
            e.status = None;
            e.redo();
            e.settle_status();
            let redone = compare(&e, &nums, &undone, &format!("{what} / redo"), seed, which);
            assert_eq!(redone, after, "seed {seed} {what}: redo did not restore rooms");
        }
        open = after;
    }
}

#[test]
fn room_fuzz_every_opened_room_named() {
    for seed in 1..=12 {
        run(seed, 12, Check::Opened);
    }
}

#[test]
fn room_fuzz_no_false_room_note() {
    for seed in 1..=12 {
        run(seed, 12, Check::NoFalse);
    }
}

#[test]
fn room_fuzz_undo_clears_note() {
    for seed in 1..=12 {
        run(seed, 12, Check::Undo);
    }
}

/// Seed 7 step 1 rotates the partition between Q103 and Q104: Q104 falls into the region of
/// Q103 and the note cites Q103 as the room that keeps the area; Q103 itself is not named.
#[test]
fn room_fuzz_seed7_dup_note_names_only_its_subject() {
    run(7, 2, Check::Undo);
    run(7, 2, Check::NoFalse);
}

#[test]
#[ignore = "100 seeds, run on demand"]
fn room_fuzz_100_seeds() {
    for seed in 1..=100 {
        run(seed, 25, Check::Undo);
    }
}
