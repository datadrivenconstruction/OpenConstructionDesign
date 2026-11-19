// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Version comparison on its own: identical copies, one-sided and two-sided differences,
//! read-only behaviour, the content hash.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::{LayerSpec, WallSpec};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "ddc-compare-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }

    fn file(&self, n: &str) -> PathBuf {
        self.0.join(n)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wall(y: f64, level: i64, t: i64) -> WallSpec {
    WallSpec {
        a: (0.0, y),
        b: (5.0, y),
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: t,
        offset0: None,
    }
}

/// A saved model with two walls, the walls' level and two wall types; a copy of it prepared for
/// comparing (the other version).
fn base(d: &Dir) -> (Model, PathBuf, [i64; 2], [i64; 2], i64) {
    let mut m = Model::open(d.file("here.ddc").to_str().unwrap()).unwrap();
    let level = m.add_level("L1", 0.0).unwrap();
    let layer = |t: f64| {
        vec![LayerSpec {
            material: "brick".into(),
            thickness: t,
        }]
    };
    let thin = m.add_wall_type("thin", &layer(0.1)).unwrap();
    let thick = m.add_wall_type("thick", &layer(0.3)).unwrap();
    let walls = [0.0, 20.0].map(|y| m.add_wall(&wall(y, level, thin)).unwrap());
    m.snapshot_to(d.file("there.ddc")).unwrap();
    let copy = prepare_compare(&d.file("there.ddc"), &d.file("cmp")).unwrap();
    (m, copy, walls, [thin, thick], level)
}

#[test]
fn identical_versions_have_no_differences() {
    let d = Dir::new();
    let (m, copy, _, _, _) = base(&d);
    let v = m.compare_version(&copy).unwrap();
    assert_eq!(v.total, 0);
    assert!(v.elements.is_empty(), "{:?}", v.elements);
    assert!(
        v.tables.iter().all(|t| t.added + t.removed + t.changed == 0),
        "{:?}",
        v.tables
    );
}

#[test]
fn each_kind_of_difference_is_listed_once_with_old_and_new_values() {
    let d = Dir::new();
    let (mut m, copy, walls, types, level) = base(&d);
    m.set_wall_type(walls[0], types[1]).unwrap();
    // add first, then delete: without AUTOINCREMENT a freed max id would be reused and pair up
    let new = m.add_wall(&wall(60.0, level, types[0])).unwrap();
    m.delete_wall(walls[1]).unwrap();
    assert!(new != walls[0] && new != walls[1], "the added id is fresh");
    let v = m.compare_version(&copy).unwrap();
    let walls_only: Vec<_> = v.elements.iter().filter(|e| e.table == "wall").collect();
    assert_eq!(walls_only.len(), 3, "{walls_only:?}");
    let kind_of = |id: i64| walls_only.iter().find(|e| e.id == id).map(|e| e.kind);
    assert_eq!(kind_of(walls[0]), Some(DiffKind::Differs));
    assert_eq!(kind_of(walls[1]), Some(DiffKind::ThereOnly));
    assert_eq!(kind_of(new), Some(DiffKind::HereOnly));
    let ch = walls_only
        .iter()
        .find(|e| e.id == walls[0])
        .unwrap()
        .changes
        .iter()
        .find(|c| c.field == "wtype")
        .expect("the type column changed");
    assert!(ch.old.is_some() && ch.new.is_some());
    assert_ne!(ch.old, ch.new, "old is the other version, new is this one");
    assert!(v.total >= 3);
    assert!(v.tables.iter().any(|t| t.table == "wall"));
}

#[test]
fn comparing_writes_nothing_and_leaves_the_other_file_alone() {
    let d = Dir::new();
    let (mut m, copy, walls, types, _) = base(&d);
    m.set_wall_type(walls[0], types[1]).unwrap();
    let hash = m.content_hash().unwrap();
    let ops: i64 = m
        .db
        .query_row("SELECT COUNT(*) FROM op", [], |r| r.get(0))
        .unwrap();
    let there = Model::open(copy.to_str().unwrap()).unwrap().content_hash().unwrap();
    let a = m.compare_version(&copy).unwrap();
    let b = m.compare_version(&copy).unwrap();
    assert_eq!(a, b, "repeatable");
    assert_eq!(m.content_hash().unwrap(), hash);
    let after: i64 = m
        .db
        .query_row("SELECT COUNT(*) FROM op", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, ops, "no journal record");
    let again = Model::open(copy.to_str().unwrap()).unwrap().content_hash().unwrap();
    assert_eq!(again, there, "the other version is unchanged");
    // no leftover attachment: a second attach under the same alias succeeds
    assert!(compare_file(m.db_path().unwrap().as_str(), &copy).is_ok());
}

#[test]
fn compare_file_matches_the_model_call() {
    let d = Dir::new();
    let (mut m, copy, walls, types, _) = base(&d);
    m.set_wall_type(walls[1], types[1]).unwrap();
    let path = m.db_path().unwrap();
    assert_eq!(compare_file(&path, &copy).unwrap(), m.compare_version(&copy).unwrap());
}

#[test]
fn missing_source_is_an_error_not_an_empty_diff() {
    let d = Dir::new();
    assert!(prepare_compare(&d.file("absent.ddc"), &d.file("cmp")).is_err());
}

#[test]
fn content_hash_follows_content_not_view_state() {
    let d = Dir::new();
    let (mut m, _, walls, types, _) = base(&d);
    let h0 = m.content_hash().unwrap();
    assert_eq!(m.content_hash().unwrap(), h0, "stable");
    m.set_wall_type(walls[0], types[1]).unwrap();
    let h1 = m.content_hash().unwrap();
    assert_ne!(h1, h0, "an edit changes it");
    m.undo().unwrap();
    assert_eq!(m.content_hash().unwrap(), h0, "undo restores it");
}

#[test]
fn only_project_content_keys_count_for_the_hash() {
    for k in ["view:plan", "pref:units", "project_uid", "phase_current", "schema_version", "view3d.style", "src_shared_a"] {
        assert!(!content_meta_key(k), "{k}");
    }
    for k in ["saved_filter:a", "title", "wall_default"] {
        assert!(content_meta_key(k), "{k}");
    }
}
