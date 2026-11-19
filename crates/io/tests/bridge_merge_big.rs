// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Re-opening a changed big source: the 3-way merge at 2k, 20k and 100k walls. Exact report
//! counts, stable ids, local edits kept, edit/delete conflicts listed, one undo step that
//! restores the whole table content, and an unchanged re-import that changes nothing.
//!
//! Layout of the walls W0..Wn (indices), for a `Plan`:
//!   [0, ed_del)            edited here, removed upstream  -> edit/delete conflict
//!   [ed_del, edited)       edited here, untouched upstream -> local edit kept
//!   [edited, edited+moved) unedited, moved upstream        -> taken from the source
//!   [edited+moved, +del)   unedited, removed upstream      -> deleted with the source
//!   plus `added` new walls in the source.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ddc_io::bridge::proto::{self, Curve, Element, Frame, Top};
use ddc_io::bridge::{self, ImportAs, Report};
use ddc_model::source::{ConflictCase, Policy};
use ddc_model::Model;

const EDITED_HEIGHT: f64 = 3.3;

fn input() -> PathBuf {
    std::env::temp_dir()
        .join("ddc-merge-big-test")
        .join("project.rvt")
}

#[derive(Clone, Copy)]
struct Plan {
    n: usize,
    edited: usize,
    ed_del: usize,
    moved: usize,
    del: usize,
    added: usize,
}

const P2K: Plan = Plan {
    n: 2_000,
    edited: 100,
    ed_del: 5,
    moved: 100,
    del: 30,
    added: 20,
};
const P20K: Plan = Plan {
    n: 20_000,
    edited: 1_000,
    ed_del: 50,
    moved: 1_000,
    del: 300,
    added: 200,
};
const P100K: Plan = Plan {
    n: 100_000,
    edited: 5_000,
    ed_del: 250,
    moved: 5_000,
    del: 1_500,
    added: 1_000,
};

fn gid(i: usize) -> String {
    format!("W{i}")
}

fn y0(i: usize) -> f64 {
    i as f64 * 0.4
}

/// The source document: walls (guid, y, height).
#[derive(Clone)]
struct Doc {
    walls: Vec<(String, f64, f64)>,
}

impl Doc {
    fn new(n: usize) -> Self {
        Doc {
            walls: (0..n).map(|i| (gid(i), y0(i), 3.0)).collect(),
        }
    }

    fn frames(&self) -> Vec<Frame> {
        let el: Vec<Element> = self
            .walls
            .iter()
            .map(|(g, y, h)| Element::Wall {
                guid: g.clone(),
                level: "L1".into(),
                wall_type: "T1".into(),
                axis: Curve::Line {
                    a: (0.0, *y),
                    b: (6.0, *y),
                },
                base_offset: 0.0,
                top: Top::Unconnected { height: *h },
                offset0: None,
            })
            .collect();
        let n = el.len() as u64;
        vec![
            Frame::Hello(proto::Hello {
                protocol: proto::PROTOCOL,
                bridge: "b".into(),
                version: "1".into(),
                formats: vec!["rvt".into()],
                capabilities: vec![],
            }),
            Frame::DocInfo(vec![("version".into(), "2024".into())]),
            Frame::Units(proto::Units { length_to_m: 1.0 }),
            Frame::Levels(vec![proto::Level {
                guid: "L1".into(),
                name: "Level 1".into(),
                elevation: 0.0,
            }]),
            Frame::Types(vec![proto::Type::Wall {
                guid: "T1".into(),
                name: "Basic 200".into(),
                layers: vec![proto::Layer {
                    material: "m".into(),
                    thickness: 0.2,
                }],
            }]),
            Frame::Elements(el),
            Frame::End(proto::Stats {
                elements: n,
                warnings: 0,
            }),
        ]
    }
}

fn first(m: &mut Model, d: &Doc) -> (Report, Duration) {
    let t = Instant::now();
    let r = bridge::import_frames(m, &input(), d.frames()).unwrap();
    (r, t.elapsed())
}

fn again(m: &mut Model, d: &Doc) -> (Report, Duration) {
    let how = ImportAs {
        policy: Policy {
            delete_removed: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let t = Instant::now();
    let r = bridge::import_frames_as(m, &input(), d.frames(), &how).unwrap();
    (r, t.elapsed())
}

fn q(m: &Model, sql: &str) -> i64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

/// guid -> model id of every imported wall.
fn wall_ids(m: &Model) -> HashMap<String, i64> {
    let mut st = m
        .sql()
        .prepare("SELECT guid, ref FROM ext_ref WHERE guid LIKE 'W%'")
        .unwrap();
    st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn height(m: &Model, id: i64) -> f64 {
    m.sql()
        .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

fn ay(m: &Model, id: i64) -> f64 {
    m.sql()
        .query_row("SELECT ay FROM wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

fn alive(m: &Model, id: i64) -> bool {
    q(m, &format!("SELECT count(*) FROM wall WHERE id={id}")) > 0
}

/// Content of the given tables; rows sorted, so the rowid order does not matter.
fn hash_of(m: &Model, tables: &[&str]) -> u64 {
    use std::hash::{Hash, Hasher};
    let db = m.sql();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for t in tables {
        let mut st = db.prepare(&format!("SELECT * FROM {t}")).unwrap();
        let n = st.column_count();
        let mut rows: Vec<String> = st
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| format!("{:?}", r.get::<_, rusqlite::types::Value>(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows.sort();
        t.hash(&mut h);
        rows.hash(&mut h);
    }
    h.finish()
}

/// Written only by the recompute; the raw SQL edits in `setup` leave them stale, so a later
/// recompute may legitimately differ from the snapshot.
const DERIVED: &[&str] = &[
    "wall_qty",
    "room_qty",
    "slab_qty",
    "member_qty",
    "roof_qty",
    "struct_qty",
    "struct_issue",
    "ceiling_qty",
    "shaft_cut",
    "stair_qty",
    "railing_qty",
    "comp_qty",
    "solid_qty",
    "solid_cut",
    "solid_base",
    "mass_qty",
    "mass_cache",
    "join_cache",
    "dim_geo",
];

/// Every content table: not the journal, derived tables or sqlite internals.
fn all_tables(m: &Model) -> Vec<String> {
    let mut st = m
        .sql()
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    let v: Vec<String> = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    v.into_iter()
        .filter(|t| t != "op" && !t.starts_with("sqlite_") && !DERIVED.contains(&t.as_str()))
        .collect()
}

fn full_hash(m: &Model) -> u64 {
    let tables = all_tables(m);
    let refs: Vec<&str> = tables.iter().map(String::as_str).collect();
    hash_of(m, &refs)
}

/// Imported, edited locally; the changed source is ready.
struct Case {
    m: Model,
    p: Plan,
    ids: HashMap<String, i64>,
    next: Doc,
    first_time: Duration,
}

impl Case {
    fn edited_range(&self) -> std::ops::Range<usize> {
        0..self.p.edited
    }
    fn moved_range(&self) -> std::ops::Range<usize> {
        self.p.edited..self.p.edited + self.p.moved
    }
    fn deleted_range(&self) -> std::ops::Range<usize> {
        let s = self.p.edited + self.p.moved;
        s..s + self.p.del
    }
}

fn setup(p: Plan) -> Case {
    let mut m = Model::open(":memory:").unwrap();
    let d = Doc::new(p.n);
    let (r, first_time) = first(&mut m, &d);
    assert_eq!(r.walls, p.n);
    let ids = wall_ids(&m);
    assert_eq!(ids.len(), p.n);

    // local edits: height of the first `edited` walls
    for i in 0..p.edited {
        m.sql()
            .execute(
                "UPDATE wall SET height=?1 WHERE id=?2",
                rusqlite::params![EDITED_HEIGHT, ids[&gid(i)]],
            )
            .unwrap();
    }

    // upstream: the next `moved` walls move, a block is removed, the first `ed_del`
    // edited walls are removed, new walls appear
    let mut next = d.clone();
    for w in &mut next.walls[p.edited..p.edited + p.moved] {
        w.1 += 0.25;
    }
    let mut gone: HashSet<String> = (0..p.ed_del).map(gid).collect();
    let s = p.edited + p.moved;
    gone.extend((s..s + p.del).map(gid));
    next.walls.retain(|w| !gone.contains(&w.0));
    for k in 0..p.added {
        next.walls.push((gid(p.n + k), y0(p.n + k), 3.0));
    }
    Case {
        m,
        p,
        ids,
        next,
        first_time,
    }
}

fn counts_and_ids(p: Plan) {
    let mut c = setup(p);
    let (r, _) = again(&mut c.m, &c.next.clone());
    assert_eq!(r.walls, p.added, "new walls created");
    assert_eq!(r.merge.added, p.added);
    assert_eq!(r.updated, p.moved, "moved walls updated in place");
    assert_eq!(r.merge.merged, p.moved);
    assert!(r.merge.taken >= p.moved, "at least one field per moved wall");
    assert_eq!(r.merge.conflicts, p.ed_del);
    assert_eq!(r.merge.deleted_upstream, p.del);
    assert_eq!(r.deleted.len(), p.del);
    assert_eq!(r.merge.pinned, 0);

    // ids of every wall that exists on both sides stay the same
    let now = wall_ids(&c.m);
    for i in (p.ed_del..p.n).filter(|i| !c.deleted_range().contains(i)) {
        assert_eq!(now.get(&gid(i)), c.ids.get(&gid(i)), "id of {}", gid(i));
    }
    for i in c.deleted_range() {
        assert!(!alive(&c.m, c.ids[&gid(i)]), "{} removed", gid(i));
    }
    for k in 0..p.added {
        assert!(now.contains_key(&gid(p.n + k)));
    }
    assert_eq!(
        q(&c.m, "SELECT count(*) FROM wall"),
        (p.n - p.del + p.added) as i64,
        "edit/delete conflicts keep their walls"
    );
}

fn local_edits_and_conflicts(p: Plan) {
    let mut c = setup(p);
    again(&mut c.m, &c.next.clone());

    for i in c.edited_range().filter(|i| *i >= p.ed_del) {
        let id = c.ids[&gid(i)];
        assert_eq!(height(&c.m, id), EDITED_HEIGHT, "{} keeps the local edit", gid(i));
        assert_eq!(ay(&c.m, id), y0(i), "{} not moved", gid(i));
    }
    for i in c.moved_range() {
        let id = c.ids[&gid(i)];
        assert_eq!(ay(&c.m, id), y0(i) + 0.25, "{} follows the source", gid(i));
        assert_eq!(height(&c.m, id), 3.0);
    }

    let cs = c.m.conflicts(None).unwrap();
    assert_eq!(cs.len(), p.ed_del);
    let listed: HashSet<i64> = cs.iter().map(|x| x.element).collect();
    let want: HashSet<i64> = (0..p.ed_del).map(|i| c.ids[&gid(i)]).collect();
    assert_eq!(listed, want, "exactly the edited-and-removed walls");
    assert!(cs
        .iter()
        .all(|x| x.case == ConflictCase::EditDelete && x.field == "*"));
    for id in want {
        assert!(alive(&c.m, id), "conflicting wall kept until resolved");
        assert_eq!(height(&c.m, id), EDITED_HEIGHT);
    }
}

fn one_undo(p: Plan) {
    let mut c = setup(p);
    let before = full_hash(&c.m);
    let (r, _) = again(&mut c.m, &c.next.clone());
    assert_eq!(r.merge.conflicts, p.ed_del);
    assert_ne!(full_hash(&c.m), before, "the merge changed something");
    c.m.undo().unwrap();
    assert_eq!(full_hash(&c.m), before, "one undo restores every table");
    assert_eq!(q(&c.m, "SELECT count(*) FROM src_conflict"), 0);
    // the local edits (outside the journal) are still there
    let id = c.ids[&gid(p.edited - 1)];
    assert_eq!(height(&c.m, id), EDITED_HEIGHT);
}

fn unchanged_noop(p: Plan) {
    let mut m = Model::open(":memory:").unwrap();
    let d = Doc::new(p.n);
    let (_, t1) = first(&mut m, &d);
    let before = hash_of(&m, &["wall", "ext_ref"]);
    let ids = wall_ids(&m);
    let (r, t2) = again(&mut m, &d);
    assert_eq!(
        (r.walls, r.updated, r.migrated, r.merge.added, r.merge.merged),
        (0, 0, 0, 0, 0)
    );
    assert_eq!(
        (r.merge.taken, r.merge.conflicts, r.merge.deleted_upstream),
        (0, 0, 0)
    );
    assert!(r.deleted.is_empty() && r.missing.is_empty());
    assert_eq!(hash_of(&m, &["wall", "ext_ref"]), before, "nothing changed");
    assert_eq!(wall_ids(&m), ids);
    eprintln!("first import {t1:?}, unchanged re-import {t2:?}");
}

#[test]
fn big_reimport_counts_and_ids_stable() {
    counts_and_ids(P2K);
}

#[test]
fn big_reimport_keeps_local_edits_and_conflicts() {
    local_edits_and_conflicts(P2K);
}

#[test]
fn big_reimport_one_undo() {
    one_undo(P2K);
}

#[test]
fn big_unchanged_reimport_noop_and_fast() {
    unchanged_noop(P2K);
}

#[test]
#[ignore = "20k walls, slow in debug builds"]
fn big_20k_reimport_counts_and_ids_stable() {
    counts_and_ids(P20K);
}

#[test]
#[ignore = "20k walls, slow in debug builds"]
fn big_20k_reimport_keeps_local_edits_and_conflicts() {
    local_edits_and_conflicts(P20K);
}

#[test]
#[ignore = "20k walls, slow in debug builds"]
fn big_20k_reimport_one_undo() {
    one_undo(P20K);
}

#[test]
#[ignore = "20k walls, slow in debug builds"]
fn big_20k_unchanged_reimport_noop_and_fast() {
    unchanged_noop(P20K);
}

#[test]
#[ignore = "release only: 100k walls"]
fn big_reimport_100k_release() {
    let mut c = setup(P100K);
    let first_time = c.first_time;
    let (r, t) = again(&mut c.m, &c.next.clone());
    assert_eq!(
        (r.walls, r.updated, r.merge.conflicts, r.merge.deleted_upstream),
        (P100K.added, P100K.moved, P100K.ed_del, P100K.del)
    );
    assert!(
        t < Duration::from_secs(60) && t < first_time * 4,
        "re-import {t:?}, first import {first_time:?}"
    );
    c.m.undo().unwrap();
    assert_eq!(q(&c.m, "SELECT count(*) FROM wall"), P100K.n as i64);
}
