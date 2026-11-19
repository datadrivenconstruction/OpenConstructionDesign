// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The per-frame read caches (keyed by `Editor::cache_key`) see every way the model goes back:
//! a rolled-back transaction, a failed atomic plan, a journal rollback, a scoped reload and a
//! new connection after a level walk.

use super::*;
use crate::editor::OpFn;
use std::sync::Arc;

fn names(ed: &Editor) -> Vec<String> {
    ed.levels().into_iter().map(|l| l.1).collect()
}

fn add_level(ed: &mut Editor, name: &'static str, z: f64) {
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.add_level(name, z).map(|_| ()))))
        .unwrap();
}

#[test]
fn rows_read_inside_a_rolled_back_transaction_are_not_kept() {
    let mut ed = Editor::new_in_memory().unwrap();
    let before = names(&ed);
    ed.model.sql().execute_batch("BEGIN").unwrap();
    ed.model.add_level("Trial", 9.0).unwrap();
    assert_eq!(names(&ed).len(), before.len() + 1, "the open transaction is read");
    ed.model.sql().execute_batch("ROLLBACK").unwrap();
    assert_eq!(names(&ed), before, "ROLLBACK keeps total_changes: the cache must not");
}

#[test]
fn a_failed_atomic_plan_leaves_the_cache_on_the_old_rows() {
    let mut ed = Editor::new_in_memory().unwrap();
    let before = names(&ed);
    let r = ed.run_op(OpFn(Arc::new(|m: &mut Model| {
        m.atomic("wall_ai_plan", |m| {
            m.add_level("Plan", 7.0)?;
            Err::<(), _>(Error::Invalid("refused".into()))
        })
    })));
    assert!(r.is_err());
    assert_eq!(names(&ed), before);
}

#[test]
fn a_journal_rollback_with_reload_is_seen() {
    let mut ed = Editor::new_in_memory().unwrap();
    let before = names(&ed);
    let top = ed.model.op_top().unwrap();
    add_level(&mut ed, "Mark", 6.0);
    assert_eq!(names(&ed).len(), before.len() + 1);
    // the AI plan rollback and the scratch copy's failed item: rollback_to, then reload
    assert_eq!(ed.model.rollback_to(top).unwrap(), 1);
    ed.reload().unwrap();
    assert_eq!(names(&ed), before);
}

#[test]
fn a_journal_rollback_with_scoped_reload_is_seen() {
    let mut ed = Editor::new_in_memory().unwrap();
    let before = names(&ed);
    let top = ed.model.op_top().unwrap();
    add_level(&mut ed, "Taken back", 6.0);
    assert_eq!(names(&ed).len(), before.len() + 1);
    // an XLSX step taken back after its check: rollback_to, then the scoped reload
    let h = ed.model.change_head();
    let rev = ed.revision;
    ed.model.rollback_to(top).unwrap();
    ed.reload_since(h).unwrap();
    assert!(ed.revision > rev, "every reload path moves the revision");
    assert_eq!(names(&ed), before);
}

#[test]
fn a_journal_rollback_without_reload_is_seen() {
    let mut ed = Editor::new_in_memory().unwrap();
    let before = names(&ed);
    let top = ed.model.op_top().unwrap();
    add_level(&mut ed, "Draft", 6.0);
    assert_eq!(names(&ed).len(), before.len() + 1);
    ed.model.rollback_to(top).unwrap();
    assert_eq!(names(&ed), before, "the undo statements move total_changes");
}

#[test]
fn a_new_model_never_reuses_a_revision_of_a_level_walk() {
    let mut ed = Editor::new_in_memory().unwrap();
    add_level(&mut ed, "Upper", 3.0);
    let before = ed.revision;
    let _ = ed.scene_all();
    assert_eq!(ed.revision, before, "the walk puts the revision back");
    let walked = ed.rev_top;
    assert!(walked > before, "the walk reloaded each level");
    let mut m = Model::open(":memory:").unwrap();
    Editor::seed_model_in(&mut m, false).unwrap();
    ed.replace_model(m).unwrap();
    assert!(ed.revision > walked, "a revision read under the old connection");
    assert_eq!(names(&ed).len(), 1, "the new model's levels");
}

#[test]
fn cache_key_is_new_on_every_call_inside_a_transaction() {
    let ed = Editor::new_in_memory().unwrap();
    assert_eq!(ed.cache_key(), ed.cache_key());
    ed.model.sql().execute_batch("BEGIN").unwrap();
    assert_ne!(ed.cache_key(), ed.cache_key());
    ed.model.sql().execute_batch("ROLLBACK").unwrap();
    assert_eq!(ed.cache_key(), ed.cache_key());
}
