// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Generic `Edit::Delete` for wall, opening and slab: create, delete, undo, redo (the same edit
//! applied again) and save/reload, per kind.

use ddc_model::{Edit, LayerSpec, Model, OpeningSpec, SlabSpec, WallSpec};

struct Fx {
    m: Model,
    wall: i64,
    opening: i64,
    slab: i64,
}

fn fixture() -> Fx {
    let mut m = Model::open(":memory:").unwrap();
    let level = m.add_level("L1", 0.0).unwrap();
    let lay = LayerSpec {
        material: "Concrete".into(),
        thickness: 0.2,
    };
    let wt = m.add_wall_type("W", &[lay.clone()]).unwrap();
    let st = m.add_slab_type("S", &[lay]).unwrap();
    let wall = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: Some(0.0),
        })
        .unwrap();
    let opening = m
        .add_opening(
            wall,
            &OpeningSpec {
                along: 3.0,
                width: 1.0,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            },
        )
        .unwrap();
    let slab = m
        .add_slab(&SlabSpec {
            level,
            slab_type: st,
            offset: 0.0,
            outer: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
            holes: vec![],
        })
        .unwrap();
    Fx {
        m,
        wall,
        opening,
        slab,
    }
}

fn count(m: &Model, table: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn del(table: &'static str, id: i64) -> [Edit; 1] {
    [Edit::Delete { table, id }]
}

fn reload(m: &Model, tag: &str) -> Model {
    let dir = std::env::temp_dir().join(format!("ddc-gdel-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dst = dir.join("p.ddc");
    m.save_project_to(&dst, &mut |_, _| Ok(())).unwrap();
    Model::open(dst.to_str().unwrap()).unwrap()
}

/// Rows of the kind and of what goes with it, as (table, count).
fn kind_counts(m: &Model, table: &str) -> Vec<(&'static str, i64)> {
    let tables: &[&'static str] = match table {
        "wall" => &["wall", "opening"],
        "opening" => &["opening"],
        _ => &["slab", "slab_vertex"],
    };
    tables.iter().map(|t| (*t, count(m, t))).collect()
}

fn id_of(fx: &Fx, table: &str) -> i64 {
    match table {
        "wall" => fx.wall,
        "opening" => fx.opening,
        _ => fx.slab,
    }
}

fn run(table: &'static str) {
    let mut fx = fixture();
    let id = id_of(&fx, table);
    let before = kind_counts(&fx.m, table);
    assert!(before.iter().all(|c| c.1 > 0), "create {table}: {before:?}");

    // saved before the delete: the element is still in the reloaded file
    let kept = reload(&fx.m, &format!("{table}-a"));
    assert_eq!(kind_counts(&kept, table), before);

    fx.m.apply_edits(&del(table, id)).unwrap();
    let gone = kind_counts(&fx.m, table);
    assert_eq!(gone[0].1, 0, "delete {table}: {gone:?}");

    fx.m.undo().unwrap();
    assert_eq!(kind_counts(&fx.m, table), before, "undo {table}");

    fx.m.apply_edits(&del(table, id)).unwrap();
    assert_eq!(kind_counts(&fx.m, table), gone, "redo {table}");

    let after = reload(&fx.m, &format!("{table}-b"));
    assert_eq!(kind_counts(&after, table), gone, "reload {table}");
}

#[test]
fn wall_generic_delete_undo_redo_reload() {
    run("wall");
}

#[test]
fn opening_generic_delete_undo_redo_reload() {
    run("opening");
}

#[test]
fn slab_generic_delete_undo_redo_reload() {
    run("slab");
}

#[test]
fn unknown_table_and_missing_row_are_refused_and_change_nothing() {
    let mut fx = fixture();
    let walls = count(&fx.m, "wall");
    assert!(fx.m.apply_edits(&del("level", 1)).is_err());
    assert!(fx.m.apply_edits(&del("wall", fx.wall + 1000)).is_err());
    assert_eq!(count(&fx.m, "wall"), walls);
}
