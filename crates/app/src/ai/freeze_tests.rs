use super::*;
use crate::ai::lower::{lower_op, lower_op_in};
use crate::ai::plan::{PlanOp, Targets as PlanTargets};
use crate::editor::ops_run::set_thread_exec;
use ddc_model::WallSpec;
use ddc_ops::{Actor, Field, Group, Unit, Value};
use std::sync::Arc;

/// Live-like editor on the real executor with `n` walls 3 m high; returns their ids.
fn walls(n: usize) -> (Editor, Vec<i64>) {
    set_thread_exec(Some(Arc::new(crate::ops_exec::ModelExec)));
    let mut ed = Editor::new_in_memory().unwrap();
    let ids = add_walls(&mut ed, n, 0.0);
    (ed, ids)
}

fn add_walls(ed: &mut Editor, n: usize, y: f64) -> Vec<i64> {
    let specs: Vec<WallSpec> = (0..n)
        .map(|k| WallSpec {
            a: (k as f64 * 5.0, y),
            b: (k as f64 * 5.0 + 4.0, y),
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ed.types[0].id,
            offset0: None,
        })
        .collect();
    let ids = ed.model.add_walls(&specs).unwrap();
    ed.reload().unwrap();
    ids
}

fn height(ed: &Editor, id: i64) -> f64 {
    ed.model
        .sql()
        .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

fn set_height(target: Targets, h: f64) -> Op {
    Op::Set {
        target,
        fields: vec![Field::new("height", Value::num(h, Unit::M))],
    }
}

fn filter(q: &str) -> Targets {
    Targets::Filter {
        cat: None,
        query: q.into(),
    }
}

fn env_of(groups: Vec<Vec<Op>>) -> Envelope {
    let mut env = Envelope::new(Actor::User, "freeze");
    for (i, ops) in groups.into_iter().enumerate() {
        env.groups.push(Group {
            id: i as u32 + 1,
            title: format!("g{i}"),
            ops,
            depends_on: vec![],
            why: String::new(),
        });
    }
    env
}

fn wall_refs(ids: &[i64]) -> Vec<Ref> {
    ids.iter().map(|&id| Ref::Id { cat: Cat::Wall, id }).collect()
}

fn frozen_refs(env: &Envelope) -> Vec<Ref> {
    match &env.groups[0].ops[0] {
        Op::Set {
            target: Targets::Refs { refs },
            ..
        } => refs.clone(),
        other => panic!("not frozen: {other:?}"),
    }
}

#[test]
fn envelope_filter_target_applies_frozen_refs() {
    let (mut ed, ids) = walls(2);
    let j0 = ed.model.journal_len().unwrap();
    let env = env_of(vec![vec![set_height(filter("walls"), 5.0)]]);
    let f = freeze(&ed, None, &env).unwrap();
    assert_eq!(frozen_refs(&f), wall_refs(&ids), "sorted ids of every match");
    ed.run_ops(env).unwrap();
    assert!(ids.iter().all(|&id| (height(&ed, id) - 5.0).abs() < 1e-9));
    assert_eq!(ed.model.journal_len().unwrap(), j0 + 1, "one journal entry");
    let h1 = ed.model.content_hash().unwrap();
    ed.undo();
    assert!(ids.iter().all(|&id| (height(&ed, id) - 3.0).abs() < 1e-9), "one undo step");
    ed.redo();
    assert_eq!(ed.model.content_hash().unwrap(), h1);
}

#[test]
fn filter_matching_nothing_refused_model_unchanged() {
    let (mut ed, _) = walls(1);
    let (h0, j0) = (ed.model.content_hash().unwrap(), ed.model.journal_len().unwrap());
    let env = env_of(vec![vec![set_height(filter("doors"), 5.0)]]);
    let r = freeze(&ed, None, &env).unwrap_err();
    assert_eq!(r.code, Code::NotFound);
    assert_eq!(r.path, "/groups/0/ops/0/target");
    assert!(r.got.as_deref().unwrap_or("").contains("doors"), "names the query: {r}");
    let e = ed.run_ops(env).unwrap_err();
    assert!(e.to_string().contains("doors"), "{e}");
    assert_eq!(ed.model.content_hash().unwrap(), h0);
    assert_eq!(ed.model.journal_len().unwrap(), j0);
    assert!(ed.preview_ops(env_of(vec![vec![set_height(filter("doors"), 5.0)]])).is_err());
}

#[test]
fn no_partial_apply_when_a_later_group_fails() {
    let (mut ed, ids) = walls(2);
    let (h0, j0) = (ed.model.content_hash().unwrap(), ed.model.journal_len().unwrap());
    // the first group resolves, the second matches nothing: nothing runs
    let empty = env_of(vec![
        vec![set_height(filter("walls"), 5.0)],
        vec![set_height(filter("doors"), 2.0)],
    ]);
    assert!(ed.run_ops(empty).is_err());
    // both resolve, the executor refuses the second (gone id): the first is rolled back too
    let gone = env_of(vec![
        vec![set_height(filter("walls"), 5.0)],
        vec![set_height(Targets::Refs { refs: wall_refs(&[9_999_999]) }, 2.0)],
    ]);
    assert!(ed.run_ops(gone).is_err());
    assert_eq!(ed.model.content_hash().unwrap(), h0);
    assert_eq!(ed.model.journal_len().unwrap(), j0);
    assert!(ids.iter().all(|&id| (height(&ed, id) - 3.0).abs() < 1e-9));
}

#[test]
fn redo_replays_frozen_filter_ids() {
    let (mut ed, ids) = walls(2);
    ed.run_ops(env_of(vec![vec![set_height(filter("walls"), 5.0)]])).unwrap();
    ed.undo();
    // a wall that appears under the redo entry must not be picked up by the replay
    let late = add_walls(&mut ed, 1, 20.0)[0];
    ed.redo();
    assert!(ids.iter().all(|&id| (height(&ed, id) - 5.0).abs() < 1e-9));
    assert!((height(&ed, late) - 3.0).abs() < 1e-9, "redo ran the frozen ids, not the filter");
}

#[test]
fn preview_freezes_and_a_changed_model_refuses_apply() {
    let (mut ed, ids) = walls(2);
    let p = ed.preview_ops(env_of(vec![vec![set_height(filter("walls"), 5.0)]])).unwrap();
    assert_eq!(frozen_refs(&p.env), wall_refs(&ids), "the preview holds the refs it showed");
    let late = add_walls(&mut ed, 1, 20.0)[0];
    let h = ed.model.content_hash().unwrap();
    assert!(ed.apply_preview(p).is_err(), "the model moved since the preview");
    assert_eq!(ed.model.content_hash().unwrap(), h);
    assert!((height(&ed, late) - 3.0).abs() < 1e-9);
    // unchanged model: Apply writes exactly the previewed ids
    let p = ed.preview_ops(env_of(vec![vec![set_height(filter("walls"), 5.0)]])).unwrap();
    let mut all = ids.clone();
    all.push(late);
    assert_eq!(frozen_refs(&p.env), wall_refs(&all));
    ed.apply_preview(p).unwrap();
    assert!(all.iter().all(|&id| (height(&ed, id) - 5.0).abs() < 1e-9));
}

#[test]
fn envelope_set_target_lowers_to_refs() {
    let (mut ed, ids) = walls(2);
    let mut sets = Sets::default();
    // given out of order and twice: the refs come back sorted and once
    let keys = vec![
        Key { cat: "wall", id: ids[1] },
        Key { cat: "wall", id: ids[0] },
        Key { cat: "wall", id: ids[1] },
    ];
    let n = sets.add(keys, vec![]);
    let del = PlanOp::Delete {
        target: PlanTargets::Set(n),
    };
    let ops = lower_op_in(&del, None, Some(&sets)).unwrap();
    assert_eq!(
        ops,
        vec![Op::Delete {
            target: Targets::Refs { refs: wall_refs(&ids) }
        }]
    );
    assert!(matches!(
        &lower_op(&del, None).unwrap()[0],
        Op::Delete { target: Targets::Set { .. } }
    ), "without sets the name stays");
    let gone = PlanOp::Delete {
        target: PlanTargets::Set(n + 1),
    };
    assert!(lower_op_in(&gone, None, Some(&sets)).is_err());
    // a set name in an envelope freezes the same way, and runs as one step
    let env = env_of(vec![vec![Op::Delete {
        target: Targets::Set { name: format!("s{n}") },
    }]]);
    let f = freeze(&ed, Some(&sets), &env).unwrap();
    assert_eq!(f.groups[0].ops, ops);
    assert_eq!(
        freeze(&ed, None, &env).unwrap_err().code,
        Code::Unsupported,
        "no sets on the live editor"
    );
    ed.run_ops(f).unwrap();
    let left: i64 = ed
        .model
        .sql()
        .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

#[test]
fn cat_restricts_filter_keys() {
    let keys = [
        Key { cat: "door", id: 4 },
        Key { cat: "window", id: 3 },
        Key { cat: "wall", id: 2 },
    ];
    let only = |c| key_refs(&keys, Some(c)).unwrap();
    assert_eq!(only(Cat::Door), vec![Ref::Id { cat: Cat::Door, id: 4 }]);
    assert_eq!(only(Cat::Opening).len(), 2, "opening takes doors and windows");
    assert_eq!(key_refs(&keys, None).unwrap().len(), 3);
}

fn wall_count(ed: &Editor) -> i64 {
    ed.model
        .sql()
        .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
        .unwrap()
}

fn set_plan(n: u32) -> crate::ai::plan::PlanState {
    let mut st = crate::ai::plan::PlanState::default();
    st.items.push(crate::ai::plan::PlanItem {
        id: 1,
        title: "delete the set".into(),
        why: String::new(),
        op: PlanOp::Delete {
            target: PlanTargets::Set(n),
        },
        checked: true,
        depends_on: vec![],
        actor: "ai".into(),
        conflict: None,
        sets: Default::default(),
    });
    st
}

#[test]
fn plan_apply_with_set_target_runs_the_session_set_as_one_undo_step() {
    use crate::ai::plan::{compile_one, CompileCx, CompiledPlan};
    let (mut live, ids) = walls(2);
    let (mut scratch, sids) = walls(2);
    assert_eq!(ids, sids, "the scratch copy has the live ids");
    let mut sets = Sets::default();
    let n = sets.add(ids.iter().map(|&id| Key { cat: "wall", id }).collect(), vec![]);
    // the worker compiles with the session's sets; Apply runs only the resolved steps
    let mut slots = std::collections::HashMap::new();
    let mut cx = CompileCx {
        ed: &mut scratch,
        slots: &mut slots,
        sets: &sets,
        gens: &[],
        rules: &[],
        declared: std::collections::HashSet::new(),
        skips: vec![],
    };
    let steps = compile_one(&mut cx, &set_plan(n).items[0].op).unwrap();
    let p = CompiledPlan {
        base_head: live.model.change_head(),
        steps: Arc::new(steps),
    };
    let top = live.model.op_top().unwrap();
    crate::ai::exec::apply(&mut live, &p).unwrap();
    assert_eq!(wall_count(&live), 0);
    assert_eq!(live.model.op_top().unwrap(), top + 1, "one journal entry");
    live.undo();
    assert_eq!(wall_count(&live), 2, "one undo step");
    assert_eq!(live.model.op_top().unwrap(), top);
}

#[test]
fn lowered_plan_set_target_needs_the_session_sets_at_apply() {
    use crate::ai::lower::{lower_plan, lower_plan_in};
    let (mut ed, ids) = walls(2);
    let mut sets = Sets::default();
    let n = sets.add(ids.iter().map(|&id| Key { cat: "wall", id }).collect(), vec![]);
    let st = set_plan(n);
    let (top, h0) = (ed.model.op_top().unwrap(), ed.model.content_hash().unwrap());
    // without the sets the set name reaches the live editor: refused, nothing journaled
    let bare = lower_plan(&st, Actor::User, "plan");
    assert!(bare.refused.is_empty());
    let err = ed.run_ops(bare.envelope.clone()).unwrap_err().to_string();
    assert!(err.contains("not available at Apply"), "{err}");
    assert_eq!(freeze(&ed, None, &bare.envelope).unwrap_err().code, Code::Unsupported);
    assert_eq!(ed.model.op_top().unwrap(), top);
    assert_eq!(ed.model.content_hash().unwrap(), h0);
    assert!(!ed.can_undo());
    // with the session's sets it lowers to refs and applies as one step
    let l = lower_plan_in(&st, Some(&sets), Actor::User, "plan");
    assert!(!needs_freeze(&l.envelope));
    ed.run_ops(l.envelope).unwrap();
    assert_eq!(wall_count(&ed), 0);
    assert_eq!(ed.model.op_top().unwrap(), top + 1, "one journal entry");
    ed.undo();
    assert_eq!(wall_count(&ed), 2, "one undo step");
}
