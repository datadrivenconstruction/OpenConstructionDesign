// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! An agent proposal as a draft: the card's counts, rows and view outline come from the real
//! executor's dry run on a copy; the live model changes only on the person's Apply, as one step.

use super::*;
use crate::ai::ui::proposal_draft::{self, DraftRow};
use crate::editor::ops_run::set_thread_exec;
use crate::editor::Editor;
use crate::ops_exec::{apply_proposal, ModelExec};
use ddc_model::{ElKey, RowKind};
use ddc_ops::{Cat, Op, Ref, Targets};
use std::collections::HashSet;

struct Live {
    ed: Editor,
    walls: Vec<i64>,
    scratch: PathBuf,
}

impl Drop for Live {
    fn drop(&mut self) {
        set_thread_exec(None);
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

fn wall_ids(ed: &Editor) -> Vec<i64> {
    let mut st = ed.model.sql().prepare("SELECT id FROM wall ORDER BY id").unwrap();
    let ids = st.query_map([], |r| r.get(0)).unwrap();
    ids.map(Result::unwrap).collect()
}

/// Two walls made through the editor, so the live model and its undo stack are real.
fn live(tag: &str) -> Live {
    set_thread_exec(Some(std::sync::Arc::new(ModelExec)));
    let mut ed = Editor::new_in_memory().unwrap();
    let segs = [
        (ddc_geom::v2(0.0, 0.0), ddc_geom::v2(4.0, 0.0)),
        (ddc_geom::v2(0.0, 6.0), ddc_geom::v2(4.0, 6.0)),
    ];
    let env = ed.wall_envelope(&segs).expect("a wall envelope");
    ed.run_ops(env).expect("two walls");
    let walls = wall_ids(&ed);
    assert_eq!(walls.len(), 2);
    let scratch = std::env::temp_dir().join(format!("ddc-ux62b-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    Live { ed, walls, scratch }
}

/// One new wall and the first wall deleted, from an agent.
fn envelope(l: &Live) -> Envelope {
    let mut env = l
        .ed
        .wall_envelope(&[(ddc_geom::v2(9.0, 0.0), ddc_geom::v2(9.0, 5.0))])
        .expect("a wall envelope");
    let next = env.groups.iter().map(|g| g.id).max().unwrap_or(0) + 1;
    env.groups.push(ddc_ops::Group {
        id: next,
        title: "drop".into(),
        ops: vec![Op::Delete {
            target: Targets::one(Ref::id(Cat::Wall, l.walls[0])),
        }],
        depends_on: vec![],
        why: String::new(),
    });
    env.actor = Actor::Agent {
        name: "mcp:agent".into(),
    };
    env
}

/// The real executor on a copy of the live file, as `ops_propose` runs it.
fn propose(l: &Live) -> (Proposals, u32, DryRun) {
    let path = l.scratch.join("scratch.db");
    l.ed.model.snapshot_to(&path).unwrap();
    let mut scratch = Model::open(&path.to_string_lossy()).unwrap();
    let env = envelope(l);
    let dry = ModelExec.dry_run(&mut scratch, &env).unwrap();
    assert!(dry.rejections.is_empty(), "{:?}", dry.rejections);
    let mut cards = Proposals::default();
    let id = cards.add("mcp:agent", env, dry.clone(), l.ed.model.change_head());
    (cards, id, dry)
}

/// What the views outline for the open cards.
fn lit(cards: &Proposals) -> HashSet<ElKey> {
    proposal_draft::open_keys(&cards.cards()).into_iter().collect()
}

fn len(diff: &J, field: &str) -> usize {
    diff[field].as_array().map_or(0, Vec::len)
}

/// Every live element the receipt names as changed or deleted, read straight from the receipt.
fn receipt_keys(diff: &J) -> HashSet<ElKey> {
    ["changed", "deleted"]
        .iter()
        .flat_map(|f| diff[*f].as_array().cloned().unwrap_or_default())
        .filter_map(|v| match Ref::parse(v.as_str()?).ok()? {
            Ref::Id { cat: Cat::Wall, id } => Some(("wall".to_string(), id)),
            _ => None,
        })
        .collect()
}

struct Snap {
    head: u64,
    undo: usize,
    walls: Vec<i64>,
}

fn snap(ed: &Editor) -> Snap {
    Snap {
        head: ed.model.change_head(),
        undo: ed.undo_len(),
        walls: wall_ids(ed),
    }
}

fn untouched(ed: &Editor, s: &Snap, why: &str) {
    assert_eq!(ed.model.change_head(), s.head, "{why}: change head");
    assert_eq!(ed.undo_len(), s.undo, "{why}: undo steps");
    assert_eq!(wall_ids(ed), s.walls, "{why}: walls");
}

#[test]
fn the_card_counts_and_rows_match_the_dry_run_and_the_model_waits() {
    let l = live("counts");
    let before = snap(&l.ed);
    let (cards, _, dry) = propose(&l);
    untouched(&l.ed, &before, "a proposal");
    let open = cards.cards();
    assert_eq!(open.len(), 1);
    let rows: Vec<DraftRow> = proposal_draft::rows(&open[0].diff);
    let n = proposal_draft::counts(&rows);
    assert_eq!(n.created, len(&dry.diff, "created") + len(&dry.diff, "copied"));
    assert_eq!(n.changed, len(&dry.diff, "changed"));
    assert_eq!(n.deleted, len(&dry.diff, "deleted"));
    assert_eq!(n.created, 1, "the one new wall: {}", dry.diff);
    assert_eq!(n.deleted, 1, "the one deleted wall: {}", dry.diff);
    let gone = rows
        .iter()
        .find(|r| r.kind == RowKind::Deleted)
        .expect("a deleted row");
    assert_eq!(gone.key, Some(("wall".to_string(), l.walls[0])));
    assert_eq!(gone.cat, "wall");
    let new = rows.iter().find(|r| r.kind == RowKind::Created).unwrap();
    assert_eq!(new.key, None, "the new wall is not in the live model, nothing to outline");
    for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
        let cat = ddc_ui::Catalog::new(lang);
        assert_eq!(
            proposal_draft::human(&cat, gone),
            format!("{} {}", cat.t("ai.proposal.el.wall"), l.walls[0])
        );
    }
    untouched(&l.ed, &before, "reading the card");
}

#[test]
fn the_view_outlines_exactly_the_affected_live_elements() {
    let l = live("lit");
    let (cards, _, dry) = propose(&l);
    let want = receipt_keys(&dry.diff);
    assert!(want.contains(&("wall".to_string(), l.walls[0])));
    assert_eq!(lit(&cards), want, "highlight = changed and deleted ids of the dry run");
    let created: HashSet<i64> = proposal_draft::rows(&dry.diff)
        .iter()
        .filter(|r| r.kind == RowKind::Created)
        .filter_map(|r| r.id)
        .collect();
    assert!(
        lit(&cards).iter().all(|k| !created.contains(&k.1) || l.walls.contains(&k.1)),
        "a created id never lights a live element"
    );
}

#[test]
fn reject_leaves_the_model_and_the_outline_clean() {
    let l = live("reject");
    let before = snap(&l.ed);
    let (mut cards, id, _) = propose(&l);
    assert!(!lit(&cards).is_empty());
    assert!(cards.reject(id));
    assert!(cards.cards().is_empty(), "the card closes");
    assert!(lit(&cards).is_empty(), "nothing stays lit");
    untouched(&l.ed, &before, "after Reject");
    assert!(!cards.reject(id), "a closed card cannot be rejected twice");
}

#[test]
fn apply_is_one_undo_step_and_clears_the_outline() {
    let mut l = live("apply");
    let before = snap(&l.ed);
    let (mut cards, id, _) = propose(&l);
    untouched(&l.ed, &before, "before Apply");
    apply_proposal(&mut l.ed, &mut cards, id).expect("applied");
    assert_eq!(cards.get(id).unwrap().state, ProposalState::Approved);
    assert!(lit(&cards).is_empty(), "an applied card lights nothing");
    assert_eq!(l.ed.undo_len(), before.undo + 1, "one undo step");
    let after = wall_ids(&l.ed);
    assert!(!after.contains(&l.walls[0]), "the wall is deleted");
    assert!(after.contains(&l.walls[1]));
    assert_eq!(after.len(), 2, "one deleted, one created");
    l.ed.undo();
    assert_eq!(l.ed.undo_len(), before.undo, "one Ctrl+Z takes it all back");
    assert_eq!(wall_ids(&l.ed), before.walls);
    assert!(apply_proposal(&mut l.ed, &mut cards, id).is_err(), "applied once only");
    assert_eq!(wall_ids(&l.ed), before.walls);
}
