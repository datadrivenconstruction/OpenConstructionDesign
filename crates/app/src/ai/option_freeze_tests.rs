// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::ai::sets::Key;
use crate::editor::ops_run::set_thread_exec;
use crate::ops_exec::apply_option;
use ddc_model::{OptionStatus, WallSpec};
use ddc_ops::{Actor, Cat, Field, Group, Op, Ref, Targets, Unit, Value};
use std::sync::Arc;

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

fn env_with(op: Op) -> Envelope {
    let mut env = Envelope::new(Actor::User, "option");
    env.groups.push(Group {
        id: 1,
        title: "g".into(),
        ops: vec![op],
        depends_on: vec![],
        why: String::new(),
    });
    env
}

fn set_height(target: Targets, h: f64) -> Envelope {
    env_with(Op::Set {
        target,
        fields: vec![Field::new("height", Value::num(h, Unit::M))],
    })
}

fn filter(q: &str) -> Targets {
    Targets::Filter {
        cat: None,
        query: q.into(),
    }
}

fn recipe(ed: &Editor, env: &Envelope) -> OptionRec {
    let (base_head, base_stamp) = ed.model.ai_option_head().unwrap();
    OptionRec {
        id: 0,
        set: "s".into(),
        name: "A".into(),
        base_head,
        base_stamp,
        envelope: env.to_json().unwrap(),
        seed: 0,
        params: "{}".into(),
        generator: Some("test".into()),
        metrics: None,
        note: None,
        why: None,
        status: OptionStatus::Draft,
    }
}

fn stored_refs(ed: &Editor, id: i64) -> Vec<Ref> {
    let rec = ed.model.ai_option_get(id).unwrap().unwrap();
    let env = Envelope::from_json(&rec.envelope).unwrap();
    match &env.groups[0].ops[0] {
        Op::Set {
            target: Targets::Refs { refs },
            ..
        } => refs.clone(),
        other => panic!("not frozen: {other:?}"),
    }
}

fn wall_refs(ids: &[i64]) -> Vec<Ref> {
    ids.iter().map(|&id| Ref::Id { cat: Cat::Wall, id }).collect()
}

fn status(ed: &Editor, id: i64) -> OptionStatus {
    ed.model.ai_option_get(id).unwrap().unwrap().status
}

#[test]
fn option_with_filter_applies_frozen_refs_as_one_undo_step() {
    let (mut ed, ids) = walls(2);
    let rec = recipe(&ed, &set_height(filter("walls"), 5.0));
    let id = add_option(Some(&ed), None, &ed.model, &rec).unwrap();
    assert_eq!(stored_refs(&ed, id), wall_refs(&ids), "the stored recipe holds the refs");
    let (top, j0) = (ed.model.op_top().unwrap(), ed.model.journal_len().unwrap());
    apply_option(&mut ed, id).unwrap();
    assert!(ids.iter().all(|&w| (height(&ed, w) - 5.0).abs() < 1e-9));
    assert_eq!(ed.model.op_top().unwrap(), top + 1, "one journal record");
    assert_eq!(ed.model.journal_len().unwrap(), j0 + 1, "one journal entry");
    assert_eq!(status(&ed, id), OptionStatus::Accepted);
    let h1 = ed.model.content_hash().unwrap();
    ed.undo();
    assert!(ids.iter().all(|&w| (height(&ed, w) - 3.0).abs() < 1e-9), "one undo step");
    assert_eq!(ed.model.op_top().unwrap(), top);
    assert_eq!(status(&ed, id), OptionStatus::Draft);
    ed.redo();
    assert_eq!(ed.model.content_hash().unwrap(), h1);
}

#[test]
fn option_with_empty_filter_refused_nothing_stored() {
    let (ed, _) = walls(1);
    let h0 = ed.model.content_hash().unwrap();
    let rec = recipe(&ed, &set_height(filter("doors"), 5.0));
    match add_option(Some(&ed), None, &ed.model, &rec) {
        Err(OptionApplyError::Rejected(r)) => {
            assert_eq!(r.code, Code::NotFound);
            assert!(r.got.as_deref().unwrap_or("").contains("doors"), "{r}");
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
    assert!(ed.model.ai_option_list(None).unwrap().is_empty());
    assert_eq!(ed.model.content_hash().unwrap(), h0);
}

#[test]
fn model_changed_after_option_shown_refused_unchanged() {
    let (mut ed, ids) = walls(2);
    let rec = recipe(&ed, &set_height(filter("walls"), 5.0));
    let id = add_option(Some(&ed), None, &ed.model, &rec).unwrap();
    let late = add_walls(&mut ed, 1, 20.0)[0];
    let (h, top) = (ed.model.content_hash().unwrap(), ed.model.op_top().unwrap());
    assert_eq!(apply_option(&mut ed, id), Err(OptionApplyError::Stale));
    assert_eq!(ed.model.content_hash().unwrap(), h);
    assert_eq!(ed.model.op_top().unwrap(), top);
    assert!(ids.iter().chain([&late]).all(|&w| (height(&ed, w) - 3.0).abs() < 1e-9));
    assert_eq!(status(&ed, id), OptionStatus::Draft);
    // freezing the old recipe on the moved model is refused too
    assert_eq!(freeze_recipe(&ed, None, &rec), Err(OptionApplyError::Stale));
}

#[test]
fn stale_set_element_refused() {
    let (mut ed, ids) = walls(2);
    let mut sets = Sets::default();
    let n = sets.add(ids.iter().map(|&id| Key { cat: "wall", id }).collect(), vec![]);
    let env = set_height(Targets::Set { name: format!("s{n}") }, 5.0);
    let ok = freeze_recipe(&ed, Some(&sets), &recipe(&ed, &env)).unwrap();
    assert!(!needs_freeze(&Envelope::from_json(&ok.envelope).unwrap()));
    ed.run_ops(env_with(Op::Delete {
        target: Targets::Refs { refs: wall_refs(&ids[1..]) },
    }))
    .unwrap();
    let h0 = ed.model.content_hash().unwrap();
    let rec = recipe(&ed, &env);
    match add_option(Some(&ed), Some(&sets), &ed.model, &rec) {
        Err(OptionApplyError::Rejected(r)) => {
            assert_eq!(r.code, Code::NotFound);
            let got = r.got.as_deref().unwrap_or("");
            assert!(got.contains(&format!("wall:{}", ids[1])), "names the gone element: {got}");
            assert!(got.contains("no longer exist"), "{got}");
            assert!(!got.contains(&format!("wall:{}", ids[0])), "only the gone one: {got}");
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
    assert!(ed.model.ai_option_list(None).unwrap().is_empty());
    assert_eq!(ed.model.content_hash().unwrap(), h0);
    assert!((height(&ed, ids[0]) - 3.0).abs() < 1e-9);
}

#[test]
fn unfrozen_stored_recipe_refused_on_apply() {
    let (mut ed, ids) = walls(1);
    let id = ed.model.ai_option_add(&recipe(&ed, &set_height(filter("walls"), 5.0))).unwrap();
    let (h, top) = (ed.model.content_hash().unwrap(), ed.model.op_top().unwrap());
    match apply_option(&mut ed, id) {
        Err(OptionApplyError::Rejected(r)) => assert_eq!(r.code, Code::Unsupported),
        other => panic!("expected a rejection, got {other:?}"),
    }
    assert_eq!(ed.model.content_hash().unwrap(), h);
    assert_eq!(ed.model.op_top().unwrap(), top);
    assert!((height(&ed, ids[0]) - 3.0).abs() < 1e-9);
    assert_eq!(status(&ed, id), OptionStatus::Draft);
}

#[test]
fn recipe_without_filter_keeps_its_text() {
    let (ed, ids) = walls(1);
    let rec = recipe(&ed, &set_height(Targets::Refs { refs: wall_refs(&ids) }, 5.0));
    assert_eq!(freeze_recipe(&ed, None, &rec).unwrap().envelope, rec.envelope);
    let gen = OptionRec {
        envelope: r#"{"op":"generate","generator":"g"}"#.into(),
        ..rec.clone()
    };
    assert_eq!(freeze_recipe(&ed, None, &gen).unwrap(), gen, "generator recipes pass through");
}

#[test]
fn producer_without_the_model_stores_only_what_needs_no_freeze() {
    let (ed, ids) = walls(1);
    let rec = recipe(&ed, &set_height(Targets::Refs { refs: wall_refs(&ids) }, 5.0));
    let gen = OptionRec {
        envelope: r#"{"op":"generate","generator":"g"}"#.into(),
        ..rec.clone()
    };
    let g = add_option(None, None, &ed.model, &gen).unwrap();
    assert_eq!(ed.model.ai_option_get(g).unwrap().unwrap().envelope, gen.envelope);
    assert_eq!(check_frozen(&ed.model, g), Ok(()), "a generator recipe is not refused at Apply");
    let r = add_option(None, None, &ed.model, &rec).unwrap();
    assert_eq!(ed.model.ai_option_get(r).unwrap().unwrap().envelope, rec.envelope);
    let unfrozen = recipe(&ed, &set_height(filter("walls"), 5.0));
    match add_option(None, None, &ed.model, &unfrozen) {
        Err(OptionApplyError::Rejected(r)) => assert_eq!(r.code, Code::Unsupported),
        other => panic!("expected a rejection, got {other:?}"),
    }
    assert_eq!(ed.model.ai_option_list(None).unwrap().len(), 2, "the refused one is not stored");
}
