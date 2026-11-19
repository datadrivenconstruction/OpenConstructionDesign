//! Parameter lock bookkeeping: a lock follows only its own parameter, and pruning is set-based.

use super::*;
use crate::{LayerSpec, ParamSpec, ParamType, WallSpec};

fn spec(name: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    }
}

/// One wall; a user parameter "height" shares its name with the built-in wall column.
fn model() -> (Model, i64, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type("w", &[LayerSpec { material: "brick".into(), thickness: 0.2 }])
        .unwrap();
    let w = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (5.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap();
    let p = m.add_param(&spec("height")).unwrap();
    (m, w, p)
}

fn key(w: i64, n: &str) -> (String, i64, String) {
    ("wall".to_string(), w, n.to_string())
}

#[test]
fn rename_user_param_keeps_builtin_field_lock() {
    let (mut m, w, p) = model();
    m.set_param_lock(&[("wall", w, "height")]).unwrap();
    m.edit_param(p, &spec("Clear")).unwrap();
    assert_eq!(m.param_locks().unwrap(), vec![key(w, "height")]);
    m.undo().unwrap();
    assert_eq!(m.param_locks().unwrap(), vec![key(w, "height")]);
}

#[test]
fn drop_user_param_keeps_builtin_field_lock() {
    let (mut m, w, p) = model();
    m.set_param_lock(&[("wall", w, "height")]).unwrap();
    m.delete_param(p).unwrap();
    assert_eq!(m.param_locks().unwrap(), vec![key(w, "height")]);
    m.undo().unwrap();
    assert_eq!(m.param_locks().unwrap(), vec![key(w, "height")]);
}

#[test]
fn rename_param_moves_its_own_lock_undo_restores() {
    let (mut m, w, p) = model();
    m.add_param(&spec("Fire")).unwrap();
    m.set_param_lock(&[("wall", w, "height"), ("wall", w, "Fire")]).unwrap();
    let f = m.params().unwrap().into_iter().find(|d| d.spec.name == "Fire").unwrap().id;
    m.edit_param(f, &spec("Rating")).unwrap();
    let mut now = m.param_locks().unwrap();
    now.sort();
    assert_eq!(now, vec![key(w, "Rating"), key(w, "height")]);
    m.undo().unwrap();
    let mut back = m.param_locks().unwrap();
    back.sort();
    assert_eq!(back, vec![key(w, "Fire"), key(w, "height")]);
    let _ = p;
}

fn raw() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(LOCK_SQL).unwrap();
    db.execute_batch(
        "CREATE TABLE a(id INTEGER PRIMARY KEY); CREATE TABLE b(id INTEGER PRIMARY KEY);
         INSERT INTO a(id) VALUES (1),(2); INSERT INTO b(id) VALUES (1),(2);",
    )
    .unwrap();
    for t in ["a", "b"] {
        for id in 1..=4 {
            for n in ["x", "y"] {
                db.execute(
                    "INSERT INTO param_lock(tbl,id,name) VALUES (?1,?2,?3)",
                    params![t, id, n],
                )
                .unwrap();
            }
        }
    }
    db
}

#[test]
fn prune_locks_one_query_per_table() {
    let db = raw();
    let (undo, stmts) = prune_tables(&db).unwrap();
    assert_eq!(stmts, 2, "16 locks, 2 tables: one statement each");
    // ids 3 and 4 of both tables are gone, two names each
    let left: i64 = db.query_row("SELECT count(*) FROM param_lock", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 8);
    drop(undo);
}

#[test]
fn prune_drops_lock_of_deleted_row_only() {
    let db = raw();
    let (undo, _) = prune_tables(&db).unwrap();
    let live: i64 = db
        .query_row("SELECT count(*) FROM param_lock WHERE tbl='a' AND id IN (1,2)", [], |r| r.get(0))
        .unwrap();
    assert_eq!(live, 4);
    drop(undo);
}

#[test]
fn prune_through_the_model_undo_restores_only_the_deleted_row() {
    let (mut m, w, _) = model();
    m.set_param_lock(&[("wall", w, "height"), ("level", 1, "name")]).unwrap();
    m.delete_wall(w).unwrap();
    assert_eq!(m.param_locks().unwrap(), vec![("level".to_string(), 1, "name".to_string())]);
    m.undo().unwrap();
    assert_eq!(m.param_locks().unwrap().len(), 2);
}
