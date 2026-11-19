// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::{LayerSpec, WallSpec};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "ddc-merge-test-{}-{}",
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

struct Common {
    level: i64,
    types: (i64, i64),
    walls: [i64; 3],
}

fn open(p: &Path) -> Model {
    Model::open(p.to_str().unwrap()).unwrap()
}

fn wall_spec(y: f64, level: i64, t: i64) -> WallSpec {
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

/// Shared history: three walls far from each other (editing one does not touch its neighbours).
fn start(d: &Dir) -> (Model, Model, Common) {
    start_with(d, |_, _| {})
}

/// [`start`] with `prep` run on the shared history just before the colleague's copy is taken.
fn start_with(d: &Dir, prep: impl FnOnce(&mut Model, &Common)) -> (Model, Model, Common) {
    let mut mine = open(&d.file("mine.ddc"));
    let level = mine.add_level("L1", 0.0).unwrap();
    let layer = |t: f64| {
        vec![LayerSpec {
            material: "brick".into(),
            thickness: t,
        }]
    };
    let t1 = mine.add_wall_type("thin", &layer(0.1)).unwrap();
    let t2 = mine.add_wall_type("thick", &layer(0.3)).unwrap();
    let walls = [0.0, 20.0, 40.0].map(|y| mine.add_wall(&wall_spec(y, level, t1)).unwrap());
    let common = Common {
        level,
        types: (t1, t2),
        walls,
    };
    prep(&mut mine, &common);
    mine.snapshot_to(&d.file("theirs.ddc")).unwrap();
    let theirs = open(&d.file("theirs.ddc"));
    (mine, theirs, common)
}

fn prepare(m: &Model, d: &Dir) -> Prepared {
    prepare_merge(
        &m.merge_lineage().unwrap(),
        &d.file("theirs.ddc"),
        &d.file("work"),
    )
    .unwrap()
}

fn axis_b(m: &Model, id: i64) -> Option<(f64, f64)> {
    m.db.query_row("SELECT bx, \"by\" FROM wall WHERE id=?1", [id], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
    .ok()
}

fn wall_type(m: &Model, id: i64) -> i64 {
    m.db.query_row("SELECT wtype FROM wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

fn count(m: &Model, t: &str) -> i64 {
    m.db.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
        .unwrap()
}

fn set_len(m: &mut Model, id: i64, y: f64, len: f64) {
    m.set_wall_axis(id, (0.0, y), (len, y)).unwrap();
}

#[test]
fn edits_on_both_sides_merge_field_by_field() {
    let d = Dir::new();
    let (mut mine, mut theirs, c) = start(&d);
    let w = c.walls[0];
    mine.set_wall_type(w, c.types.1).unwrap();
    set_len(&mut theirs, w, 0.0, 7.0);
    let added = theirs
        .add_wall(&wall_spec(60.0, c.level, c.types.0))
        .unwrap();
    theirs.delete_wall(c.walls[2]).unwrap();
    drop(theirs);

    let p = prepare(&mine, &d);
    let plan = mine.merge_preview(&p).unwrap();
    assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
    assert_eq!((plan.added, plan.changed, plan.deleted), (1, 1, 1));
    let rep = mine.merge_apply(&p, &plan, &[]).unwrap();
    assert_eq!((rep.added, rep.changed, rep.deleted), (1, 1, 1));

    assert_eq!(wall_type(&mine, w), c.types.1, "mine stays");
    assert_eq!(axis_b(&mine, w), Some((7.0, 0.0)), "theirs arrives");
    assert_eq!(axis_b(&mine, c.walls[2]), None);
    assert_eq!(count(&mine, "wall"), 3);
    let _ = added;
    mine.assert_integrity();
}

#[test]
fn same_field_changed_twice_is_a_conflict_with_per_element_choice() {
    for (pick, want) in [(Pick::Mine, 6.0), (Pick::Theirs, 7.0)] {
        let d = Dir::new();
        let (mut mine, mut theirs, c) = start(&d);
        let w = c.walls[0];
        set_len(&mut mine, w, 0.0, 6.0);
        set_len(&mut theirs, w, 0.0, 7.0);
        drop(theirs);
        let p = prepare(&mine, &d);
        let plan = mine.merge_preview(&p).unwrap();
        assert_eq!(plan.conflicts.len(), 1, "{plan:?}");
        let cf = &plan.conflicts[0];
        assert_eq!(
            (cf.table.as_str(), cf.id, cf.case),
            ("wall", w, MergeCase::EditEdit)
        );
        assert!(cf.fields.iter().any(|f| f.field == "bx"));
        let rep = mine
            .merge_apply(&p, &plan, &[("wall".to_string(), w, pick)])
            .unwrap();
        assert_eq!(rep.kept_mine + rep.took_theirs, 1);
        assert_eq!(axis_b(&mine, w), Some((want, 0.0)));
        mine.assert_integrity();
    }
}

#[test]
fn delete_there_versus_edit_here() {
    for (pick, alive) in [(Pick::Mine, true), (Pick::Theirs, false)] {
        let d = Dir::new();
        let (mut mine, mut theirs, c) = start(&d);
        let w = c.walls[1];
        set_len(&mut mine, w, 20.0, 6.0);
        theirs.delete_wall(w).unwrap();
        drop(theirs);
        let p = prepare(&mine, &d);
        let plan = mine.merge_preview(&p).unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].case, MergeCase::DeletedThere);
        assert_eq!(plan.deleted, 0, "not deleted without a decision");
        mine.merge_apply(&p, &plan, &[("wall".to_string(), w, pick)])
            .unwrap();
        assert_eq!(axis_b(&mine, w).is_some(), alive);
        mine.assert_integrity();
    }
}

#[test]
fn delete_here_versus_edit_there_restores_on_request() {
    for (pick, alive) in [(Pick::Mine, false), (Pick::Theirs, true)] {
        let d = Dir::new();
        let (mut mine, mut theirs, c) = start(&d);
        let w = c.walls[1];
        mine.delete_wall(w).unwrap();
        set_len(&mut theirs, w, 20.0, 7.0);
        drop(theirs);
        let p = prepare(&mine, &d);
        let plan = mine.merge_preview(&p).unwrap();
        assert_eq!(plan.conflicts[0].case, MergeCase::DeletedHere);
        let before = mine.content_hash().unwrap();
        mine.merge_apply(&p, &plan, &[("wall".to_string(), w, pick)])
            .unwrap();
        assert_eq!(axis_b(&mine, w).is_some(), alive);
        if alive {
            assert_eq!(axis_b(&mine, w), Some((7.0, 20.0)));
        }
        mine.assert_integrity();
        mine.undo().unwrap();
        assert_eq!(mine.content_hash().unwrap(), before);
    }
}

#[test]
fn undo_restores_the_exact_hash_in_one_step() {
    let d = Dir::new();
    let (mut mine, mut theirs, c) = start(&d);
    set_len(&mut mine, c.walls[0], 0.0, 6.0);
    mine.add_wall(&wall_spec(80.0, c.level, c.types.0)).unwrap();
    set_len(&mut theirs, c.walls[1], 20.0, 7.0);
    theirs.delete_wall(c.walls[2]).unwrap();
    theirs
        .add_wall(&wall_spec(60.0, c.level, c.types.1))
        .unwrap();
    drop(theirs);

    let before = mine.content_hash().unwrap();
    let ops = count(&mine, "op");
    let p = prepare(&mine, &d);
    let plan = mine.merge_preview(&p).unwrap();
    assert_eq!(
        mine.content_hash().unwrap(),
        before,
        "preview writes nothing"
    );
    mine.merge_apply(&p, &plan, &[]).unwrap();
    assert_ne!(mine.content_hash().unwrap(), before);
    assert_eq!(count(&mine, "op"), ops + 1);
    mine.undo().unwrap();
    assert_eq!(mine.content_hash().unwrap(), before);
    mine.assert_integrity();
}

#[test]
fn new_ids_do_not_collide_with_work_on_this_side() {
    let d = Dir::new();
    let (mut mine, mut theirs, c) = start(&d);
    let here = mine.add_wall(&wall_spec(80.0, c.level, c.types.1)).unwrap();
    let there = theirs
        .add_wall(&wall_spec(60.0, c.level, c.types.0))
        .unwrap();
    assert_eq!(
        here, there,
        "the same fresh id on both sides is the hard case"
    );
    drop(theirs);
    let p = prepare(&mine, &d);
    let plan = mine.merge_preview(&p).unwrap();
    assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
    assert_eq!(plan.added, 1);
    mine.merge_apply(&p, &plan, &[]).unwrap();
    assert_eq!(count(&mine, "wall"), 5);
    assert_eq!(axis_b(&mine, here), Some((5.0, 80.0)));
    mine.assert_integrity();
}

#[test]
fn repeat_merge_adds_no_copies() {
    let d = Dir::new();
    let (mut mine, mut theirs, c) = start(&d);
    theirs
        .add_wall(&wall_spec(60.0, c.level, c.types.0))
        .unwrap();
    theirs.snapshot_to(&d.file("t1.ddc")).unwrap();
    let p = prepare_merge(
        &mine.merge_lineage().unwrap(),
        &d.file("t1.ddc"),
        &d.file("w1"),
    )
    .unwrap();
    let plan = mine.merge_preview(&p).unwrap();
    mine.merge_apply(&p, &plan, &[]).unwrap();
    assert_eq!(count(&mine, "wall"), 4);

    let p = prepare_merge(
        &mine.merge_lineage().unwrap(),
        &d.file("t1.ddc"),
        &d.file("w2"),
    )
    .unwrap();
    assert!(p.peer.is_some());
    let plan = mine.merge_preview(&p).unwrap();
    assert!(plan.nothing, "{plan:?}");

    // the peer keeps working: only the new changes arrive
    theirs
        .add_wall(&wall_spec(70.0, c.level, c.types.0))
        .unwrap();
    theirs.snapshot_to(&d.file("t2.ddc")).unwrap();
    let p = prepare_merge(
        &mine.merge_lineage().unwrap(),
        &d.file("t2.ddc"),
        &d.file("w3"),
    )
    .unwrap();
    let plan = mine.merge_preview(&p).unwrap();
    assert_eq!((plan.added, plan.changed, plan.deleted), (1, 0, 0));
    mine.merge_apply(&p, &plan, &[]).unwrap();
    assert_eq!(count(&mine, "wall"), 5);
    mine.assert_integrity();
}

#[test]
fn stale_preview_is_refused() {
    let d = Dir::new();
    let (mut mine, mut theirs, c) = start(&d);
    set_len(&mut theirs, c.walls[0], 0.0, 7.0);
    drop(theirs);
    let p = prepare(&mine, &d);
    let plan = mine.merge_preview(&p).unwrap();
    set_len(&mut mine, c.walls[1], 20.0, 6.0);
    assert!(matches!(
        mine.merge_apply(&p, &plan, &[]),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn unrelated_file_is_refused() {
    let d = Dir::new();
    let mine = open(&d.file("mine.ddc"));
    let other = open(&d.file("other.ddc"));
    drop(other);
    let res = prepare_merge(
        &mine.merge_lineage().unwrap(),
        &d.file("other.ddc"),
        &d.file("work"),
    );
    assert!(matches!(res, Err(MergeError::NoCommonBase)));
}

#[test]
fn colleague_file_is_not_touched() {
    let d = Dir::new();
    let (mut mine, mut theirs, c) = start(&d);
    set_len(&mut theirs, c.walls[0], 0.0, 7.0);
    let their_hash = theirs.content_hash().unwrap();
    drop(theirs);
    let p = prepare(&mine, &d);
    let plan = mine.merge_preview(&p).unwrap();
    mine.merge_apply(&p, &plan, &[]).unwrap();
    assert_eq!(
        open(&d.file("theirs.ddc")).content_hash().unwrap(),
        their_hash
    );
}

#[test]
fn version_compare_lists_elements_with_a_place() {
    let d = Dir::new();
    let (mut mine, mut theirs, c) = start(&d);
    set_len(&mut mine, c.walls[0], 0.0, 6.0);
    theirs.delete_wall(c.walls[1]).unwrap();
    let there = theirs
        .add_wall(&wall_spec(60.0, c.level, c.types.0))
        .unwrap();
    drop(theirs);
    let copy = crate::prepare_compare(&d.file("theirs.ddc"), &d.file("cmp")).unwrap();
    let v = mine.compare_version(&copy).unwrap();
    let find = |id: i64| {
        v.elements
            .iter()
            .find(|e| e.table == "wall" && e.id == id)
            .unwrap()
    };
    let changed = find(c.walls[0]);
    assert_eq!(changed.kind, crate::DiffKind::Differs);
    assert!(changed.fields.iter().any(|f| f == "bx"));
    assert_eq!(changed.spot.unwrap().level, Some(c.level));
    assert_eq!(find(c.walls[1]).kind, crate::DiffKind::HereOnly);
    let new = find(there);
    assert_eq!(new.kind, crate::DiffKind::ThereOnly);
    assert_eq!(new.spot.unwrap().min, [0.0, 60.0]);
    assert!(v.total >= 3);
    assert!(v.tables.iter().any(|t| t.table == "wall"));
}

/// AI bookkeeping (audit log, consents, option recipes) is not model content: compare, merge and
/// the content hash ignore it. The snapshot table counts still list options a restore would drop.
#[test]
fn ai_bookkeeping_is_not_a_model_difference() {
    let d = Dir::new();
    let (mine, theirs, _) = start(&d);
    drop(theirs);
    let h0 = mine.content_hash().unwrap();
    mine.db
        .execute_batch(
            "INSERT INTO ai_audit(ts, session, actor, event) VALUES (1, 's', 'user', 'ask');
             INSERT INTO ai_policy(key, value) VALUES ('k', 'v');
             INSERT INTO ai_option(set_name, name, base_head, envelope) VALUES ('s', 'a', 0, '{}');
             INSERT INTO ai_option_merge(option_id, seq, stamp) VALUES (1, 1, 0);",
        )
        .unwrap();
    assert_eq!(mine.content_hash().unwrap(), h0);
    let copy = crate::prepare_compare(&d.file("theirs.ddc"), &d.file("cmp")).unwrap();
    let v = mine.compare_version(&copy).unwrap();
    assert!(
        !v.elements.iter().any(|e| e.table.starts_with("ai_")),
        "{:?}",
        v.elements.iter().map(|e| &e.table).collect::<Vec<_>>()
    );
    assert!(v.tables.iter().any(|t| t.table == "ai_option"));
}

#[test]
fn merge_remaps_bool_ids() {
    use crate::{BoolRequest, Form, MassSpec};
    use ddc_geom::csg::BoolOp;
    let d = Dir::new();
    let (mut mine, mut theirs, c) = start(&d);
    let cube = |lo, hi| MassSpec {
        level: c.level,
        name: "box".into(),
        void: false,
        inplace: false,
        form: Form::Cuboid { lo, hi },
    };
    let cut = |m: &mut Model, x: f64| {
        let a = m.add_mass(&cube((x, 0., 0.), (x + 4., 4., 3.))).unwrap();
        let b = m
            .add_mass(&cube((x + 1., 1., -1.), (x + 2., 2., 4.)))
            .unwrap();
        let r = m
            .apply_boolean(&BoolRequest {
                op: BoolOp::Subtract,
                a,
                b,
                name: None,
            })
            .unwrap();
        (a, b, r)
    };
    // both sides add forms with the same fresh ids: the colleague's ids move
    let here = cut(&mut mine, 0.0);
    let there = cut(&mut theirs, 100.0);
    assert_eq!(here, there, "the same fresh ids on both sides");
    drop(theirs);
    let p = prepare(&mine, &d);
    let plan = mine.merge_preview(&p).unwrap();
    assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
    mine.merge_apply(&p, &plan, &[]).unwrap();

    assert_eq!(count(&mine, "mass"), 6);
    assert_eq!(count(&mine, "mass_bool"), 2);
    assert_eq!(
        mine.boolean_link(here.2).unwrap().map(|l| (l.a, l.b)),
        Some((here.0, here.1)),
        "own link is untouched"
    );
    let theirs_r = mine
        .masses()
        .unwrap()
        .into_iter()
        .filter(|&r| r != here.2)
        .find_map(|r| mine.boolean_link(r).unwrap())
        .expect("colleague link arrives");
    for id in [theirs_r.result, theirs_r.a, theirs_r.b] {
        assert!(![here.0, here.1, here.2].contains(&id), "{id} is remapped");
        assert!(mine.mass(id).is_ok());
    }
    // the link still points at the colleague's own cubes (x from 100), not at ours
    let lo = |id| mine.mass_body(id).unwrap().bbox().unwrap().0.x;
    assert!(lo(theirs_r.a) > 50.0 && lo(theirs_r.b) > 50.0 && lo(theirs_r.result) > 50.0);
    assert_eq!(
        mine.boolean_dependents(theirs_r.a).unwrap(),
        vec![theirs_r.result]
    );
    mine.assert_integrity();
}

/// Parameter locks are this project's settings, not content: the content hash and version
/// compare ignore them, and a merge keeps this side's locks and takes none of the colleague's.
#[test]
fn compare_ignores_param_lock_rows() {
    let d = Dir::new();
    let (mut mine, mut theirs, c) = start(&d);
    set_len(&mut theirs, c.walls[1], 20.0, 8.0);
    theirs.set_param_lock(&[("wall", c.walls[2], "Mark")]).unwrap();
    drop(theirs);
    let h0 = mine.content_hash().unwrap();
    mine.set_param_lock(&[("wall", c.walls[0], "Fire")]).unwrap();
    assert_eq!(mine.content_hash().unwrap(), h0, "a lock is not content");
    let copy = crate::prepare_compare(&d.file("theirs.ddc"), &d.file("cmp")).unwrap();
    let v = mine.compare_version(&copy).unwrap();
    assert!(
        !v.elements.iter().any(|e| e.table == "param_lock"),
        "{:?}",
        v.elements.iter().map(|e| &e.table).collect::<Vec<_>>()
    );
    assert!(!v.tables.iter().any(|t| t.table == "param_lock"));
    assert!(v.elements.iter().any(|e| e.table == "wall" && e.id == c.walls[1]));
    let p = prepare(&mine, &d);
    let plan = mine.merge_preview(&p).unwrap();
    mine.merge_apply(&p, &plan, &[]).unwrap();
    assert_eq!(axis_b(&mine, c.walls[1]), Some((8.0, 20.0)));
    assert!(mine.param_locked("wall", c.walls[0], "Fire").unwrap(), "own lock kept");
    assert!(!mine.param_locked("wall", c.walls[2], "Mark").unwrap(), "no lock taken");
}

/// An apply inside an atomic plan (an open transaction) cannot DETACH: the next merge must not
/// stop on the alias left attached; each apply is one undo step.
#[test]
fn merge_apply_inside_a_step_then_again() {
    let d = Dir::new();
    let (mut mine, mut theirs, c) = start(&d);
    for (k, y) in [(1, 60.0), (2, 70.0)] {
        theirs
            .add_wall(&wall_spec(y, c.level, c.types.0))
            .unwrap();
        let t = d.file(&format!("s{k}.ddc"));
        theirs.snapshot_to(&t).unwrap();
        let p = prepare_merge(
            &mine.merge_lineage().unwrap(),
            &t,
            &d.file(&format!("sw{k}")),
        )
        .unwrap();
        let plan = mine.merge_preview(&p).unwrap();
        if k == 1 {
            mine.atomic("wall_merge", |m| m.merge_apply(&p, &plan, &[]))
                .unwrap();
        } else {
            let (_, wrote) = mine.step(|m| m.merge_apply(&p, &plan, &[])).unwrap();
            assert!(wrote);
        }
        assert_eq!(count(&mine, "wall"), 3 + k);
    }
    mine.undo().unwrap();
    assert_eq!(count(&mine, "wall"), 4, "one undo step per apply");
    mine.assert_integrity();
}

fn fire_edit(id: i64, v: &str) -> crate::Edit {
    crate::Edit::ParamValue {
        param: "Fire".into(),
        kind: "wall".into(),
        id,
        value: Some(crate::PVal::Text(v.into())),
    }
}

fn fire_value(m: &Model, id: i64) -> Option<crate::PVal> {
    let p = m.params().unwrap().into_iter().find(|d| d.spec.name == "Fire").unwrap().id;
    m.param_value(p, "wall", id).unwrap()
}

/// A colleague's parameter value never overwrites a locked value here; unlocked ones merge.
#[test]
fn merge_keeps_locked_parameter_value() {
    let d = Dir::new();
    let text = |v: &str| Some(crate::PVal::Text(v.into()));
    let (mut mine, mut theirs, c) = start_with(&d, |m, c| {
        m.add_param(&crate::ParamSpec {
            name: "Fire".into(),
            ptype: crate::ParamType::Text,
            per_type: false,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["wall".into()],
        })
        .unwrap();
        m.apply_edits(&[fire_edit(c.walls[0], "EI15"), fire_edit(c.walls[1], "EI15")]).unwrap();
    });
    theirs
        .apply_edits(&[fire_edit(c.walls[0], "EI90"), fire_edit(c.walls[1], "EI90")])
        .unwrap();
    drop(theirs);
    mine.set_param_lock(&[("wall", c.walls[0], "Fire")]).unwrap();
    let p = prepare(&mine, &d);
    let plan = mine.merge_preview(&p).unwrap();
    let rep = mine.merge_apply(&p, &plan, &[]).unwrap();
    assert_eq!(fire_value(&mine, c.walls[0]), text("EI15"), "locked value kept");
    assert_eq!(fire_value(&mine, c.walls[1]), text("EI90"), "unlocked value merged");
    assert_eq!(rep.locked, 1);
    assert!(mine.param_locked("wall", c.walls[0], "Fire").unwrap());
}
