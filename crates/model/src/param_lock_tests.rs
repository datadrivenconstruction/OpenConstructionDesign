//! Parameter lock: a locked value is kept by every batch path and by the value writer itself.

use crate::{Edit, LayerSpec, Model, PVal, ParamSet, ParamSpec, ParamType, WallSpec};

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

/// Two walls, parameters "Fire" and "Mark" bound to walls.
fn model() -> (Model, i64, i64, i64, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type(
            "w",
            &[LayerSpec {
                material: "brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let wall = |m: &mut Model, y: f64| {
        m.add_wall(&WallSpec {
            a: (0.0, y),
            b: (5.0, y),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap()
    };
    let (w1, w2) = (wall(&mut m, 0.0), wall(&mut m, 3.0));
    let fire = m.add_param(&spec("Fire")).unwrap();
    let mark = m.add_param(&spec("Mark")).unwrap();
    (m, w1, w2, fire, mark)
}

fn set(param: &str, id: i64, v: &str) -> Edit {
    Edit::ParamValue {
        param: param.into(),
        kind: "wall".into(),
        id,
        value: Some(PVal::Text(v.into())),
    }
}

fn text(v: &str) -> Option<PVal> {
    Some(PVal::Text(v.into()))
}

#[test]
fn lock_one_undo() {
    let (mut m, w1, _, _, _) = model();
    assert!(!m.param_locked("wall", w1, "Fire").unwrap());
    assert!(m.param_locks().unwrap().is_empty());
    assert_eq!(m.set_param_lock(&[("wall", w1, "Fire")]).unwrap(), 1);
    assert!(m.param_locked("wall", w1, "Fire").unwrap());
    // locking twice is not a second step
    assert!(m.set_param_lock(&[("wall", w1, "Fire")]).is_err());
    m.undo().unwrap();
    assert!(!m.param_locked("wall", w1, "Fire").unwrap());

    m.set_param_lock(&[("wall", w1, "Fire")]).unwrap();
    assert_eq!(m.clear_param_lock(&[("wall", w1, "Fire")]).unwrap(), 1);
    assert!(!m.param_locked("wall", w1, "Fire").unwrap());
    assert!(m.clear_param_lock(&[("wall", w1, "Fire")]).is_err());
    m.undo().unwrap();
    assert!(m.param_locked("wall", w1, "Fire").unwrap());
    assert_eq!(
        m.param_locks().unwrap(),
        vec![("wall".to_string(), w1, "Fire".to_string())]
    );
}

#[test]
fn match_properties_keeps_locked_value() {
    let (mut m, w1, w2, fire, mark) = model();
    m.apply_edits(&[set("Fire", w1, "EI90"), set("Mark", w1, "A"), set("Fire", w2, "EI15")])
        .unwrap();
    m.set_param_lock(&[("wall", w2, "Fire")]).unwrap();
    m.match_properties(("wall", w1), &[("wall", w2)]).unwrap();
    assert_eq!(m.param_value(fire, "wall", w2).unwrap(), text("EI15"));
    assert_eq!(m.param_value(mark, "wall", w2).unwrap(), text("A"));
}

#[test]
fn unlock_on_a_model_without_locks_is_refused() {
    let (mut m, w1, _, _, _) = model();
    assert!(m.clear_param_lock(&[("wall", w1, "Fire")]).is_err());
}

#[test]
fn split_locked_takes_out_only_the_locked_value() {
    let (mut m, w1, w2, _, _) = model();
    let batch = vec![set("Fire", w1, "EI60"), set("Mark", w1, "A"), set("Fire", w2, "EI30")];
    // no lock yet: everything stays
    let (keep, skipped) = m.split_locked(&batch).unwrap();
    assert_eq!((keep.len(), skipped.len()), (3, 0));
    m.set_param_lock(&[("wall", w1, "Fire")]).unwrap();
    let (keep, skipped) = m.split_locked(&batch).unwrap();
    assert_eq!(keep.len(), 2);
    assert_eq!(skipped, vec![("wall".to_string(), w1, "Fire".to_string())]);
    assert!(keep
        .iter()
        .all(|e| !matches!(e, Edit::ParamValue { param, id, .. } if param == "Fire" && *id == w1)));
}

#[test]
fn unlocked_param_still_written() {
    let (mut m, w1, w2, fire, mark) = model();
    m.apply_edits(&[set("Fire", w1, "EI15")]).unwrap();
    m.set_param_lock(&[("wall", w1, "Fire")]).unwrap();
    // the same parameter of another wall and another parameter of the same wall
    m.apply_edits(&[set("Fire", w2, "EI30"), set("Mark", w1, "A")])
        .unwrap();
    assert_eq!(m.param_value(fire, "wall", w2).unwrap(), text("EI30"));
    assert_eq!(m.param_value(mark, "wall", w1).unwrap(), text("A"));
    assert_eq!(m.param_value(fire, "wall", w1).unwrap(), text("EI15"));
    // unlocked again: the value is written
    m.clear_param_lock(&[("wall", w1, "Fire")]).unwrap();
    m.apply_edits(&[set("Fire", w1, "EI90")]).unwrap();
    assert_eq!(m.param_value(fire, "wall", w1).unwrap(), text("EI90"));
}

#[test]
fn locked_value_is_refused_by_the_writer_and_nothing_changes() {
    let (mut m, w1, w2, fire, _) = model();
    m.apply_edits(&[set("Fire", w1, "EI15")]).unwrap();
    m.set_param_lock(&[("wall", w1, "Fire")]).unwrap();
    let e = m
        .apply_edits(&[set("Fire", w2, "EI30"), set("Fire", w1, "EI60")])
        .unwrap_err();
    assert!(e.to_string().contains("locked"), "{e}");
    // one transaction: the unlocked half did not land either
    assert_eq!(m.param_value(fire, "wall", w2).unwrap(), None);
    assert_eq!(m.param_value(fire, "wall", w1).unwrap(), text("EI15"));
    let cell = ParamSet {
        param: fire,
        kind: "wall".into(),
        id: w1,
        value: None,
    };
    assert!(m.set_param_values(&[cell]).is_err());
    assert_eq!(m.param_value(fire, "wall", w1).unwrap(), text("EI15"));
}

#[test]
fn type_value_lock_is_on_the_type_row() {
    let (mut m, w1, _, _, _) = model();
    let mut s = spec("Rating");
    s.per_type = true;
    let p = m.add_param(&s).unwrap();
    let (tk, tid) = m.type_of("wall", w1).unwrap().unwrap();
    let edit = |v: &str| Edit::ParamValue {
        param: "Rating".into(),
        kind: tk.into(),
        id: tid,
        value: text(v),
    };
    m.apply_edits(&[edit("R1")]).unwrap();
    m.set_param_lock(&[(tk, tid, "Rating")]).unwrap();
    let (keep, skipped) = m.split_locked(&[edit("R2")]).unwrap();
    assert!(keep.is_empty());
    assert_eq!(skipped, vec![(tk.to_string(), tid, "Rating".to_string())]);
    assert!(m.apply_edits(&[edit("R2")]).is_err());
    assert_eq!(m.param_value(p, tk, tid).unwrap(), text("R1"));
}

fn locked(m: &Model) -> Vec<(String, i64, String)> {
    m.param_locks().unwrap()
}

#[test]
fn rename_param_keeps_lock() {
    let (mut m, w1, w2, fire, _) = model();
    m.apply_edits(&[set("Fire", w1, "EI90")]).unwrap();
    m.set_param_lock(&[("wall", w1, "Fire"), ("wall", w2, "Fire")]).unwrap();
    m.edit_param(fire, &spec("Rating")).unwrap();
    assert!(!m.param_locked("wall", w1, "Fire").unwrap());
    assert!(m.param_locked("wall", w1, "Rating").unwrap());
    assert!(m.param_locked("wall", w2, "Rating").unwrap());
    // the renamed parameter's value is still protected
    assert!(m.apply_edits(&[set("Rating", w1, "EI15")]).is_err());
    assert_eq!(m.param_value(fire, "wall", w1).unwrap(), text("EI90"));
    // one undo step brings the old name and the old lock names back
    m.undo().unwrap();
    assert!(m.param_locked("wall", w1, "Fire").unwrap());
    assert!(!m.param_locked("wall", w1, "Rating").unwrap());
    assert_eq!(locked(&m).len(), 2);
}

#[test]
fn delete_param_drops_its_locks() {
    let (mut m, w1, _, fire, _) = model();
    m.set_param_lock(&[("wall", w1, "Fire"), ("wall", w1, "Mark")]).unwrap();
    m.delete_param(fire).unwrap();
    assert_eq!(locked(&m), vec![("wall".to_string(), w1, "Mark".to_string())]);
    // a parameter created later under the same name starts unlocked
    m.add_param(&spec("Fire")).unwrap();
    assert!(!m.param_locked("wall", w1, "Fire").unwrap());
    m.undo().unwrap();
    m.undo().unwrap();
    assert!(m.param_locked("wall", w1, "Fire").unwrap());
    assert_eq!(locked(&m).len(), 2);
}

#[test]
fn deleted_element_lock_pruned_undo_restores() {
    let (mut m, w1, w2, _, _) = model();
    m.set_param_lock(&[("wall", w1, "Fire"), ("wall", w2, "Fire")]).unwrap();
    m.delete_wall(w2).unwrap();
    assert_eq!(locked(&m), vec![("wall".to_string(), w1, "Fire".to_string())]);
    m.undo().unwrap();
    assert!(m.param_locked("wall", w2, "Fire").unwrap());
    assert_eq!(locked(&m).len(), 2);
}

#[test]
fn reused_rowid_not_locked() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type("w", &[LayerSpec { material: "brick".into(), thickness: 0.2 }])
        .unwrap();
    let spec_at = |y: f64| WallSpec {
        a: (0.0, y),
        b: (5.0, y),
        level: lv,
        base_offset: 0.0,
        height: 3.0,
        wall_type: t,
        offset0: None,
    };
    m.add_param(&spec("Fire")).unwrap();
    let w = m.add_wall(&spec_at(0.0)).unwrap();
    m.set_param_lock(&[("wall", w, "Fire")]).unwrap();
    m.delete_wall(w).unwrap();
    let again = m.add_wall(&spec_at(9.0)).unwrap();
    assert_eq!(again, w, "the last row id is reused");
    assert!(!m.param_locked("wall", again, "Fire").unwrap());
    m.apply_edits(&[set("Fire", again, "EI30")]).unwrap();
}

/// The lock table is an ordered schema step: a fresh model has it before any lock, and a file
/// of the previous version gets it on open.
#[test]
fn schema_has_param_lock_step() {
    let has = |m: &Model| -> i64 {
        m.db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='param_lock'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let m = Model::open(":memory:").unwrap();
    assert_eq!(has(&m), 1);
    assert!(m.param_locks().unwrap().is_empty());
    let dir = std::env::temp_dir().join(format!("ddc-lock-step-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("old.ddc");
    let p = p.to_str().unwrap();
    {
        let m = Model::open(p).unwrap();
        m.db.execute_batch("DROP TABLE param_lock").unwrap();
        m.db.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            [crate::schema::VERSION - 1],
        )
        .unwrap();
    }
    let m = Model::open(p).unwrap();
    assert_eq!(has(&m), 1, "the step adds the table to an older file");
    let v: i64 = m
        .db
        .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, crate::schema::VERSION);
    drop(m);
    let _ = std::fs::remove_dir_all(&dir);
}
