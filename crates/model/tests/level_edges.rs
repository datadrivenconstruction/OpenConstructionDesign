//! Level edge cases: refused edits leave the model untouched, undo/redo restores level state.

use ddc_model::{ConstraintKind, ConstraintSpec, Error, LayerSpec, Model, WallSpec};
use rusqlite::types::Value;

fn state(m: &Model) -> Vec<String> {
    let mut out = vec![];
    for q in [
        "SELECT * FROM level ORDER BY id",
        "SELECT * FROM wall ORDER BY id",
    ] {
        let mut st = m.sql().prepare(q).unwrap();
        let n = st.column_count();
        let rows = st
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| format!("{:?}", r.get::<_, Value>(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap();
        out.extend(rows.map(|r| r.unwrap()));
    }
    out
}

fn heights(m: &Model) -> Vec<f64> {
    let mut st = m.sql().prepare("SELECT height FROM wall ORDER BY id").unwrap();
    let v = st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    v
}

/// L1 at 0, L2 at 3; one wall on L1 with its top tied to L2.
fn topped() -> (Model, i64, i64, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let t = m
        .add_wall_type("W", &[LayerSpec { material: "c".into(), thickness: 0.2 }])
        .unwrap();
    let w = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            level: l1,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap();
    m.add_constraint(&ConstraintSpec {
        kind: ConstraintKind::TopToLevel,
        walls: vec![w, l2],
        value: None,
    })
    .unwrap();
    (m, l1, l2, w)
}

#[test]
fn level_raise_past_top_level_refused() {
    let (mut m, l1, _l2, _w) = topped();
    let before = state(&m);
    for z in [3.0, 5.0] {
        let e = m.update_level(l1, "L1", z).unwrap_err();
        assert!(matches!(e, Error::Conflict(_)), "{e}");
        assert_eq!(state(&m), before, "refused edit writes nothing");
    }
    assert!(heights(&m).iter().all(|h| *h > 0.0));
    m.update_level(l1, "L1", 1.0).unwrap();
    assert_eq!(heights(&m), vec![2.0]);
}

#[test]
fn delete_into_wall_topped_by_target_keeps_height() {
    let (mut m, l1, l2, _w) = topped();
    let before = state(&m);
    match m.delete_level_into(l1, l2) {
        Err(_) => assert_eq!(state(&m), before, "refused delete writes nothing"),
        Ok(()) => {
            let h = heights(&m);
            assert!(h.iter().all(|h| *h > 0.0), "no zero or negative height: {h:?}");
            assert_eq!(h, vec![3.0], "wall keeps its height");
        }
    }
}

#[test]
fn rename_to_existing_name_refused() {
    let (mut m, l1, _l2, _w) = topped();
    let before = state(&m);
    let e = m.update_level(l1, "L2", 0.0).unwrap_err();
    assert!(matches!(e, Error::Invalid(_)), "{e}");
    assert!(e.to_string().contains("already exists"), "{e}");
    assert_eq!(state(&m), before);
    // same name on the same level is not a clash
    m.update_level(l1, "L1", 0.5).unwrap();
}

#[test]
fn level_edit_undo_redo_hash() {
    let (mut m, l1, _l2, _w) = topped();
    let s0 = state(&m);
    m.update_level(l1, "Ground", -1.0).unwrap();
    let s1 = state(&m);
    assert_ne!(s0, s1);
    m.undo().unwrap();
    assert_eq!(state(&m), s0);
    m.update_level(l1, "Ground", -1.0).unwrap();
    assert_eq!(state(&m), s1);
    m.undo().unwrap();
    assert_eq!(state(&m), s0);
}
