// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::ai::exec::{created_keys, fill, ModelStep};
use crate::ai::plan::{attribute, split_reused, PlanState};
use crate::ai::session::open_editor;
use ddc_model::{Edit, OptionRec, OptionStatus, PVal, ParamSpec, ParamType, WallSpec};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

const LIMIT: usize = 200_000;

/// The live model, its base copy (as the assistant takes it of an unsaved model) and its walls.
struct Fx {
    live: Editor,
    base: PathBuf,
    walls: Vec<i64>,
    tag: String,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.base);
        let _ = std::fs::remove_file(work_of(&self.tag));
    }
}

fn path_of(tag: &str, what: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ddc-xs2-{}-{tag}-{what}.db", std::process::id()))
}

fn work_of(tag: &str) -> PathBuf {
    path_of(tag, "work")
}

fn spec(ed: &Editor, x: f64, h: f64) -> WallSpec {
    WallSpec {
        a: (x, 0.0),
        b: (x + 4.0, 0.0),
        level: ed.level,
        base_offset: 0.0,
        height: h,
        wall_type: ed.types[0].id,
        offset0: None,
    }
}

fn fixture(tag: &str, n: usize) -> Fx {
    let mut live = Editor::new_in_memory().unwrap();
    let specs: Vec<WallSpec> = (0..n).map(|k| spec(&live, k as f64 * 5.0, 3.0)).collect();
    let walls = live.model.add_walls(&specs).unwrap();
    live.reload().unwrap();
    let fx = Fx {
        live,
        base: path_of(tag, "base"),
        walls,
        tag: tag.to_string(),
    };
    resync(&fx);
    fx
}

/// A new base copy of the live model, as a sync or the promote after an Apply makes it.
fn resync(fx: &Fx) {
    let _ = std::fs::remove_file(&fx.base);
    fx.live.model.snapshot_to(&fx.base).unwrap();
}

/// The draft as the worker builds it: steps filled on a copy of the base, the card diffed with
/// the base. Frozen against the live revision of now.
fn draft(fx: &Fx, mut steps: Vec<ModelStep>) -> (CompiledPlan, Vec<ElemDiff>, bool) {
    let work = work_of(&fx.tag);
    let _ = std::fs::remove_file(&work);
    std::fs::copy(&fx.base, &work).unwrap();
    let mut scratch = open_editor(&work, fx.live.tool.units).unwrap();
    let h = scratch.model.change_head();
    for s in &mut steps {
        fill(&mut scratch.model, s).unwrap();
    }
    let touched = scratch.model.changes_since(h);
    let raw = scratch.model.diff_from(&fx.base, &touched, LIMIT).unwrap();
    let reused: HashSet<ElKey> = steps.iter().flat_map(created_keys).collect();
    let card = split_reused(group_diff(&raw), &reused);
    let plan = CompiledPlan {
        base_head: fx.live.model.change_head(),
        steps: Arc::new(steps),
    };
    (plan, card, raw.truncated)
}

fn shown<'a>(base: &'a Path, card: &'a [ElemDiff], truncated: bool) -> Shown<'a> {
    Shown {
        base,
        diff: card,
        truncated,
        limit: LIMIT,
    }
}

fn audit() -> AuditEntry {
    AuditEntry {
        session: "xs2".into(),
        actor: "user".into(),
        ..AuditEntry::default()
    }
}

fn next_wall(ed: &Editor) -> i64 {
    ed.model
        .sql()
        .query_row("SELECT coalesce(max(id),0)+1 FROM wall", [], |r| r.get(0))
        .unwrap()
}

fn new_walls(fx: &Fx, xs: &[f64]) -> ModelStep {
    ModelStep::Walls {
        specs: xs.iter().map(|&x| spec(&fx.live, x, 3.0)).collect(),
        expect: vec![],
    }
}

fn set_height(fx: &Fx, k: usize, h: f64) -> ModelStep {
    ModelStep::Edits(vec![Edit::Wall {
        id: fx.walls[k],
        spec: spec(&fx.live, k as f64 * 5.0, h),
        bulge: 0.0,
    }])
}

fn height(ed: &Editor, id: i64) -> f64 {
    ed.model
        .sql()
        .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

/// Content hash, journal length, undo depth: what a taken-back Apply leaves as it found.
fn state(ed: &Editor) -> (u64, usize, usize) {
    (
        ed.model.content_hash().unwrap(),
        ed.model.journal_len().unwrap(),
        ed.undo_len(),
    )
}

fn diverged_entries(ed: &Editor) -> Vec<AuditEntry> {
    ed.model
        .audit_list(0, 10_000)
        .unwrap()
        .into_iter()
        .map(|(_, _, e)| e)
        .filter(|e| e.event == "apply_check" && e.status == "diverged")
        .collect()
}

#[test]
fn honest_apply_passes_and_is_one_undo_step() {
    let mut fx = fixture("honest", 2);
    let (p, card, tr) = draft(&fx, vec![new_walls(&fx, &[20.0, 30.0]), set_height(&fx, 0, 5.0)]);
    assert!(!card.is_empty(), "the card shows the draft");
    let s0 = state(&fx.live);
    let next = next_wall(&fx.live);
    let r = apply_verified(&mut fx.live, &p, None, &shown(&fx.base,&card, tr), &audit(), false);
    assert_eq!(r, Ok(None));
    assert!((height(&fx.live, fx.walls[0]) - 5.0).abs() < 1e-9);
    assert_eq!(next_wall(&fx.live), next + 2, "both new walls are in");
    let s1 = state(&fx.live);
    assert_eq!((s1.1, s1.2), (s0.1 + 1, s0.2 + 1), "one journal entry, one undo step");
    assert!(diverged_entries(&fx.live).is_empty());
    fx.live.undo();
    assert_eq!(state(&fx.live), s0, "Undo takes the whole Apply back");
}

#[test]
fn a_divergent_apply_is_taken_back_with_an_audit_entry() {
    let mut fx = fixture("diverge", 2);
    let (_, card, tr) = draft(&fx, vec![set_height(&fx, 0, 5.0)]);
    // stands in for an Apply bug: the step written is not the one the card was made of
    let (wrong, _, _) = draft(&fx, vec![set_height(&fx, 0, 6.0)]);
    let s0 = state(&fx.live);
    let top = fx.live.model.op_top().unwrap();
    let r = apply_verified(&mut fx.live, &wrong, None, &shown(&fx.base,&card, tr), &audit(), false);
    let msg = match r {
        Err(AiError::Diverged(m)) => m,
        other => panic!("not taken back: {other:?}"),
    };
    assert!(msg.contains(&format!("wall {}", fx.walls[0])), "names the element: {msg}");
    assert!(!msg.contains("apply_verify."), "a message, not a key: {msg}");
    assert_eq!(state(&fx.live), s0, "model, journal and undo as before");
    assert_eq!(fx.live.model.op_top().unwrap(), top);
    assert!(!fx.live.can_redo(), "the step is not offered again by Redo");
    assert!((height(&fx.live, fx.walls[0]) - 3.0).abs() < 1e-9);
    let e = diverged_entries(&fx.live);
    assert_eq!(e.len(), 1, "one audit entry");
    assert_eq!(e[0].op_seq, None);
    assert!(e[0].text.as_deref().unwrap_or("").contains("wall"), "{:?}", e[0].text);
}

#[test]
fn an_extra_element_the_card_does_not_show_is_taken_back() {
    let mut fx = fixture("extra", 1);
    let (_, card, tr) = draft(&fx, vec![new_walls(&fx, &[20.0])]);
    let (wrong, _, _) = draft(&fx, vec![new_walls(&fx, &[20.0, 30.0])]);
    let s0 = state(&fx.live);
    let next = next_wall(&fx.live);
    let r = apply_verified(&mut fx.live, &wrong, None, &shown(&fx.base,&card, tr), &audit(), false);
    assert!(matches!(r, Err(AiError::Diverged(_))), "{r:?}");
    assert_eq!(state(&fx.live), s0);
    assert_eq!(next_wall(&fx.live), next, "no wall stays behind");
    assert_eq!(diverged_entries(&fx.live).len(), 1);
}

fn option(ed: &Editor, name: &str) -> i64 {
    let (base_head, base_stamp) = ed.model.ai_option_head().unwrap();
    ed.model
        .ai_option_add(&OptionRec {
            id: 0,
            set: "Entrance".into(),
            name: name.into(),
            base_head,
            base_stamp,
            envelope: "{}".into(),
            seed: 0,
            params: "{}".into(),
            generator: None,
            metrics: None,
            note: None,
            why: None,
            status: OptionStatus::Draft,
        })
        .unwrap()
}

fn opt_status(ed: &Editor, id: i64) -> (OptionStatus, Option<String>) {
    let r = ed.model.ai_option_get(id).unwrap().unwrap();
    (r.status, r.note)
}

/// Parts of two options on one merge card, then the second option on its own after the sync
/// the first Apply causes: each Apply is checked, each merged mark rides its own journal entry.
#[test]
fn merged_options_are_checked_like_any_draft() {
    let mut fx = fixture("merge", 2);
    let a = option(&fx.live, "A");
    let b = option(&fx.live, "B");
    resync(&fx);
    let (p, card, tr) = draft(&fx, vec![new_walls(&fx, &[20.0]), set_height(&fx, 1, 4.0)]);
    let note = Some("A with the taller back wall of B".to_string());
    let r = apply_verified(&mut fx.live, &p, Some((a, note.clone())), &shown(&fx.base,&card, tr), &audit(), false);
    assert_eq!(r, Ok(None));
    assert_eq!(opt_status(&fx.live, a), (OptionStatus::Merged, note));
    resync(&fx);
    let (p2, card2, tr2) = draft(&fx, vec![set_height(&fx, 0, 4.5)]);
    let r = apply_verified(&mut fx.live, &p2, Some((b, None)), &shown(&fx.base,&card2, tr2), &audit(), false);
    assert_eq!(r, Ok(None));
    assert_eq!(opt_status(&fx.live, b).0, OptionStatus::Merged);
    fx.live.undo();
    assert_eq!(opt_status(&fx.live, b).0, OptionStatus::Draft, "undo reverts the second mark only");
    assert_eq!(opt_status(&fx.live, a).0, OptionStatus::Merged);
}

#[test]
fn a_divergent_merge_leaves_the_option_unmerged() {
    let mut fx = fixture("merge-bad", 1);
    let a = option(&fx.live, "A");
    resync(&fx);
    let (_, card, tr) = draft(&fx, vec![new_walls(&fx, &[20.0])]);
    let (wrong, _, _) = draft(&fx, vec![new_walls(&fx, &[25.0])]);
    let s0 = state(&fx.live);
    let r = apply_verified(&mut fx.live, &wrong, Some((a, Some("x".into()))), &shown(&fx.base,&card, tr), &audit(), false);
    assert!(matches!(r, Err(AiError::Diverged(_))), "{r:?}");
    assert_eq!(state(&fx.live), s0);
    assert_eq!(opt_status(&fx.live, a), (OptionStatus::Draft, None), "the mark went back with the step");
}

/// The person edits the model after the draft was frozen: Apply refuses before writing (the
/// session then takes a new copy and re-previews), even when the edit was undone again.
#[test]
fn an_edit_after_the_draft_refuses_apply() {
    let mut fx = fixture("stale", 1);
    let (p, card, tr) = draft(&fx, vec![set_height(&fx, 0, 5.0)]);
    fx.live.model.add_walls(&[spec(&fx.live, 40.0, 3.0)]).unwrap();
    fx.live.reload().unwrap();
    let s0 = state(&fx.live);
    let r = apply_verified(&mut fx.live, &p, None, &shown(&fx.base,&card, tr), &audit(), false);
    assert_eq!(r, Err(AiError::Stale));
    assert_eq!(state(&fx.live), s0, "nothing written");
    assert!((height(&fx.live, fx.walls[0]) - 3.0).abs() < 1e-9);

    let mut fx = fixture("stale-undo", 1);
    let (p, card, tr) = draft(&fx, vec![set_height(&fx, 0, 5.0)]);
    let next = next_wall(&fx.live);
    let w = spec(&fx.live, 40.0, 3.0);
    fx.live
        .run_op(crate::editor::OpFn(Arc::new(move |m: &mut Model| m.add_walls(std::slice::from_ref(&w)).map(|_| ()))))
        .unwrap();
    fx.live.undo();
    assert_eq!(next_wall(&fx.live), next, "the edit is undone");
    let s0 = state(&fx.live);
    let r = apply_verified(&mut fx.live, &p, None, &shown(&fx.base,&card, tr), &audit(), false);
    assert_eq!(r, Err(AiError::Stale), "an edit and its undo still refuse: never applied blindly");
    assert_eq!(state(&fx.live), s0);
    assert!(diverged_entries(&fx.live).is_empty());
}

#[test]
fn no_base_copy_refuses_before_writing() {
    let mut fx = fixture("nobase", 1);
    let (p, card, tr) = draft(&fx, vec![set_height(&fx, 0, 5.0)]);
    let gone = path_of("nobase", "missing");
    let s0 = state(&fx.live);
    let sh = Shown {
        base: &gone,
        diff: &card,
        truncated: tr,
        limit: LIMIT,
    };
    let r = apply_verified(&mut fx.live, &p, None, &sh, &audit(), false);
    assert!(matches!(r, Err(AiError::Diverged(_))), "{r:?}");
    assert_eq!(state(&fx.live), s0);
    assert_eq!(diverged_entries(&fx.live).len(), 1);
}

fn add_type_param(ed: &mut Editor, name: &str) {
    ed.model
        .add_param(&ParamSpec {
            name: name.into(),
            ptype: ParamType::Text,
            per_type: true,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["wall".into()],
        })
        .unwrap();
}

/// A type parameter set through a wall writes the wall's type: the card shows that type row
/// with every wall of the type, and the check of the honest Apply agrees with it.
#[test]
fn a_type_param_via_an_element_shows_the_type_and_applies() {
    let mut fx = fixture("type", 3);
    add_type_param(&mut fx.live, "Acoustic class");
    fx.live.reload().unwrap();
    resync(&fx);
    let wt: i64 = fx
        .live
        .model
        .sql()
        .query_row("SELECT wtype FROM wall WHERE id=?1", [fx.walls[0]], |r| r.get(0))
        .unwrap();
    let step = ModelStep::Edits(vec![Edit::ParamValue {
        param: "Acoustic class".into(),
        kind: "wall_type".into(),
        id: wt,
        value: Some(PVal::Text("B".into())),
    }]);
    let (p, mut card, tr) = draft(&fx, vec![step]);
    let key: ElKey = ("wall_type".into(), wt);
    assert!(card.iter().any(|e| e.key == key), "the type row is on the card: {card:?}");
    assert!(!card.iter().any(|e| e.key.0 == "wall"), "no wall row changes");
    assert_eq!(type_users(&fx.live.model, &key), Some(3), "every wall of the type");
    assert_eq!(type_users(&fx.live.model, &("wall".into(), fx.walls[0])), None);
    attribute(&mut card, &PlanState::default(), &fx.live);
    let row = card.iter().find(|e| e.key == key).unwrap();
    assert_eq!(Some(row.label.clone()), type_row_label(&fx.live.model, &key), "the label counts the walls");
    let r = apply_verified(&mut fx.live, &p, None, &shown(&fx.base,&card, tr), &audit(), false);
    assert_eq!(r, Ok(None));
    let d = fx.live.model.param_by_name("Acoustic class").unwrap().unwrap();
    assert_eq!(fx.live.model.param_value_of(&d, "wall_type", wt).unwrap(), Some(PVal::Text("B".into())));
}

fn row(key: (&str, i64), kind: RowKind, col: &str, after: &str) -> ElemDiff {
    ElemDiff {
        key: (key.0.to_string(), key.1),
        kind,
        item: None,
        level: None,
        label: String::new(),
        cells: vec![ddc_model::CellDiff {
            col: col.into(),
            before: None,
            after: Some(after.into()),
        }],
    }
}

#[test]
fn first_difference_names_what_differs() {
    let a = vec![row(("wall", 1), RowKind::Changed, "height", "5")];
    assert_eq!(first_difference(&a, &a), None);
    assert_eq!(
        first_difference(&a, &[]),
        Some(Divergence::Missing { key: ("wall".into(), 1), kind: RowKind::Changed })
    );
    let extra = vec![a[0].clone(), row(("wall", 2), RowKind::Created, "height", "3")];
    assert_eq!(
        first_difference(&a, &extra),
        Some(Divergence::Extra { key: ("wall".into(), 2), kind: RowKind::Created })
    );
    let other = vec![row(("wall", 1), RowKind::Changed, "height", "6")];
    assert_eq!(
        first_difference(&a, &other),
        Some(Divergence::Value { key: ("wall".into(), 1), col: "height".into() })
    );
    let gone = vec![row(("wall", 1), RowKind::Deleted, "height", "5")];
    assert!(matches!(first_difference(&a, &gone), Some(Divergence::Kind { .. })));
}

const KEYS: &[&str] = &[
    "apply_verify.err.diverged",
    "apply_verify.rolled_back",
    "apply_verify.diff.missing",
    "apply_verify.diff.extra",
    "apply_verify.diff.kind",
    "apply_verify.diff.value",
    "apply_verify.diff.truncated",
    "apply_verify.diff.unreadable",
    "apply_verify.no_base",
    "apply_verify.take_back_failed",
    "apply_verify.kind.created",
    "apply_verify.kind.changed",
    "apply_verify.kind.deleted",
    "apply_verify.card.type_row",
];

/// Both languages are read explicitly: the global UI language is never switched here.
#[test]
fn messages_exist_in_en_and_ru() {
    for k in KEYS {
        let en = crate::tr_lang(false, k, &[]);
        let ru = crate::tr_lang(true, k, &[]);
        assert!(!en.is_empty() && en != *k, "{k} en");
        assert!(!ru.is_empty() && ru != *k, "{k} ru");
        assert_ne!(en, ru, "{k}: ru falls back to en");
    }
    let d = Divergence::Value { key: ("wall".into(), 7), col: "height".into() };
    let (en, ru) = (d.text(false), d.text(true));
    assert!(en.contains("wall 7") && ru.contains("wall 7"), "{en} / {ru}");
    assert_ne!(en, ru);
    assert_eq!(
        crate::ai::ui::logic::classify(&AiError::Diverged(en)),
        crate::ai::ui::logic::ErrKind::Diverged
    );
}
