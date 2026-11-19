// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Delete of migrated families: dependents are reported and confirmed like the old path,
//! one undo step restores everything.

use crate::editor::delete_deps::QUIET_MAX;
use crate::editor::{structx, Editor};
use crate::modify_tools::tests::walls;
use ddc_model::{FootingSpec, FootingStep, RaftSpec, RoomSpec};

fn footing(e: &mut Editor) -> i64 {
    let level = e.level;
    let id = e
        .model
        .add_footing(&FootingSpec {
            level,
            column: None,
            at: (1.0, 1.0),
            rotation: 0.0,
            round: false,
            steps: vec![FootingStep { w: 1.2, l: 1.2, h: 0.5 }],
            top_offset: 0.0,
            material: "Concrete".to_string(),
        })
        .unwrap();
    e.reload().unwrap();
    id
}

fn raft(e: &mut Editor) -> i64 {
    let level = e.level;
    let id = e
        .model
        .add_raft(&RaftSpec {
            level,
            ring: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 6.0), (0.0, 6.0)],
            thickness: 0.4,
            edge_width: 0.0,
            edge_depth: 0.0,
            top_offset: 0.0,
            material: "Concrete".to_string(),
        })
        .unwrap();
    e.reload().unwrap();
    id
}

/// `sets` rebar sets and one cover on a host of `kind`.
fn rebar(e: &Editor, kind: &str, host: i64, sets: usize) {
    for _ in 0..sets {
        e.model
            .sql()
            .execute(
                "INSERT INTO rebar_set(host_kind,host,layout,rtype) VALUES (?1,?2,'x',1)",
                rusqlite::params![kind, host],
            )
            .unwrap();
    }
    e.model
        .sql()
        .execute(
            "INSERT INTO rebar_cover(host_kind,host,face,cover) VALUES (?1,?2,'top',0.05)",
            rusqlite::params![kind, host],
        )
        .unwrap();
}

fn count(e: &Editor, table: &str) -> i64 {
    e.model
        .sql()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn delete_footing_reports_rebar_sets() {
    let mut e = Editor::new_in_memory().unwrap();
    let f = footing(&mut e);
    rebar(&e, "footing", f, 2);
    e.selection = vec![structx::sel("footing", f).unwrap()];
    let steps = e.undo_len();
    e.delete_selection();
    assert!(e.pending_delete.is_none(), "three dependents on the level go quietly");
    assert_eq!(count(&e, "iso_footing"), 0);
    assert_eq!(count(&e, "rebar_set") + count(&e, "rebar_cover"), 0);
    let s = e.status.clone().unwrap();
    assert!(s.contains(&crate::tr_n("del.k.rebar", 3)), "{s}");
    assert_eq!(e.undo_len(), steps + 1);
}

#[test]
fn delete_raft_confirm_lists_rebar() {
    let mut e = Editor::new_in_memory().unwrap();
    let r = raft(&mut e);
    rebar(&e, "raft", r, QUIET_MAX);
    e.selection = vec![structx::sel("raft", r).unwrap()];
    let steps = e.undo_len();
    e.delete_selection();
    let p = e.pending_delete.as_ref().expect("asks");
    // raft has no family code, so it stays on the classic delete path
    assert!(!p.via_elem);
    assert_eq!(p.deps.counts, vec![("rebar", QUIET_MAX + 1)]);
    assert_eq!(count(&e, "raft"), 1);
    assert_eq!(e.undo_len(), steps);
    assert!(e.confirm_delete());
    assert_eq!(count(&e, "raft"), 0);
    assert_eq!(count(&e, "rebar_set"), 0);
    assert_eq!(e.undo_len(), steps + 1);
}

#[test]
fn undo_restores_all_dependents() {
    let mut e = Editor::new_in_memory().unwrap();
    let r = raft(&mut e);
    rebar(&e, "raft", r, QUIET_MAX);
    let before = (count(&e, "raft"), count(&e, "raft_vertex"), count(&e, "rebar_set"), count(&e, "rebar_cover"));
    e.selection = vec![structx::sel("raft", r).unwrap()];
    e.delete_selection();
    assert!(e.confirm_delete());
    assert_eq!(count(&e, "rebar_set") + count(&e, "rebar_cover"), 0);
    e.undo();
    let after = (count(&e, "raft"), count(&e, "raft_vertex"), count(&e, "rebar_set"), count(&e, "rebar_cover"));
    assert_eq!(after, before);
}

#[test]
fn exact_room_count_only_bounded_rooms() {
    let mut e = Editor::new_in_memory().unwrap();
    let boxed = |x: f64| {
        [
            ((x, 0.0), (x + 4.0, 0.0)),
            ((x + 4.0, 0.0), (x + 4.0, 4.0)),
            ((x + 4.0, 4.0), (x, 4.0)),
            ((x, 4.0), (x, 0.0)),
        ]
    };
    walls(&mut e, &boxed(0.0));
    walls(&mut e, &boxed(20.0));
    let level = e.level;
    for x in [2.0, 22.0] {
        e.model
            .add_room(&RoomSpec {
                level,
                name: "R".into(),
                number: "1".into(),
                point: (x, 2.0),
                height: 3.0,
            })
            .unwrap();
    }
    e.reload().unwrap();
    let w = e.walls[0].id; // walls are listed in creation order: a wall of the first box
    let items = vec![("wall".to_string(), w)];
    let d = crate::editor::delete_deps::collect_exact(&e.model, &items, level);
    assert_eq!(d.rooms, 1, "the other box keeps its room out of the count");
}
