// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::ai::exec;
use crate::ai::plan::{self, NewElem};
use crate::editor::Editor;

const BASE: (i64, i64) = (4, 40);

fn wall(ed: &Editor, y: f64) -> NewElem {
    NewElem::Wall {
        level: ed.level,
        a: (0.0, y),
        b: (6.0, y),
        bulge: 0.0,
        base_offset: 0.0,
        height: 3.0,
        wtype: None,
    }
}

fn door(on: ElRef) -> NewElem {
    NewElem::Opening {
        wall: on,
        along: 2.0,
        width: 0.9,
        height: 2.1,
        sill: 0.0,
        kind: "door".into(),
    }
}

fn item(id: u32, op: PlanOp, depends_on: Vec<u32>) -> PlanItem {
    PlanItem {
        id,
        title: format!("item {id}"),
        why: String::new(),
        op,
        checked: true,
        depends_on,
        actor: "ai".into(),
        conflict: None,
        sets: Default::default(),
    }
}

fn compile(st: &mut PlanState, ed: &mut Editor) {
    plan::replay(st, ed, &Sets::default(), &[], &[]).unwrap();
}

/// The option: 1 a wall, 2 a door in that wall, 3 another wall; compiled on its own copy.
fn option(ed: &mut Editor) -> PlanState {
    let mut st = PlanState::default();
    let w = st.reserve_slots(1, 1);
    let first = vec![wall(ed, 0.0)];
    st.items.push(item(1, PlanOp::Create { slot0: w, items: first }, vec![]));
    let d = st.reserve_slots(1, 2);
    let doors = vec![door(ElRef::new("wall", w))];
    st.items.push(item(2, PlanOp::Create { slot0: d, items: doors }, vec![1]));
    let w2 = st.reserve_slots(1, 3);
    let second = vec![wall(ed, 5.0)];
    st.items.push(item(3, PlanOp::Create { slot0: w2, items: second }, vec![]));
    st.next_item = 3;
    compile(&mut st, ed);
    assert!(st.items.iter().all(|i| i.conflict.is_none()), "the option compiles");
    st
}

fn source<'a>(st: &'a PlanState, sets: &'a Sets, status: OptionStatus) -> MergeSource<'a> {
    MergeSource {
        option: 7,
        status,
        base: BASE,
        plan: st,
        sets,
    }
}

fn opening_of(st: &PlanState, item: u32) -> ElKey {
    st.declared[&item]
        .iter()
        .find(|k| k.0 == "opening")
        .cloned()
        .expect("the item made a door")
}

fn count(ed: &Editor, table: &str) -> i64 {
    ed.model
        .sql()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn settle(card: &mut MergeCard) {
    let st = std::mem::take(&mut card.plan);
    after_replay(card, &st);
    card.plan = st;
}

#[test]
fn merge_parts_includes_host_wall() {
    let mut opt = Editor::new_in_memory().unwrap();
    let st = option(&mut opt);
    let sets = Sets::default();
    let src = source(&st, &sets, OptionStatus::Draft);
    let door = opening_of(&st, 2);
    let pick = Pick::Elements(vec![door.clone()]);
    let mut card = merge_card(&src, &pick, BASE, MergeOpts::default(), (0, 0), None).unwrap();
    assert_eq!(card.kept, vec![1, 2], "the door brings its host wall");
    assert_eq!(card.pulled, vec![(1, 2)]);
    assert_eq!(card.left_out, vec![(3, Reason::NotPicked)], "what stays out says why");
    assert!(!card.rebased);

    let strict = MergeOpts {
        pull_deps: false,
        rebase: true,
    };
    let refused = merge_card(&src, &pick, BASE, strict, (0, 0), None).err();
    assert_eq!(refused, Some(Refusal::NeedsLeftOut { item: 2, needs: 1 }));
    let stray = Pick::Elements(vec![("wall".to_string(), 99_999)]);
    let refused = merge_card(&src, &stray, BASE, MergeOpts::default(), (0, 0), None).err();
    assert_eq!(refused, Some(Refusal::NotInOption(("wall".to_string(), 99_999))));
    let rejected = source(&st, &sets, OptionStatus::Rejected);
    let refused = merge_card(&rejected, &pick, BASE, MergeOpts::default(), (0, 0), None).err();
    assert_eq!(refused, Some(Refusal::NotMergeable(OptionStatus::Rejected)));

    let mut scratch = Editor::new_in_memory().unwrap();
    let (walls, doors) = (count(&scratch, "wall"), count(&scratch, "opening"));
    compile(&mut card.plan, &mut scratch);
    settle(&mut card);
    assert_eq!(card.kept, vec![1, 2]);
    assert_eq!(count(&scratch, "wall"), walls + 1, "only the host wall, not wall 3");
    assert_eq!(count(&scratch, "opening"), doors + 1);
}

#[test]
fn merge_is_one_undo_step() {
    let mut opt = Editor::new_in_memory().unwrap();
    let st = option(&mut opt);
    let sets = Sets::default();
    let src = source(&st, &sets, OptionStatus::Draft);
    let note = Some("door faces the street".to_string());
    let pick = Pick::Items(vec![2]);
    let mut card = merge_card(&src, &pick, BASE, MergeOpts::default(), (10, 20), note).unwrap();
    assert_eq!(card.kept, vec![1, 2]);
    let ids: Vec<(u32, bool)> = card.plan.items.iter().map(|i| (i.id, i.checked)).collect();
    assert_eq!(ids, vec![(11, true), (12, true), (13, false)], "numbers follow the session's");
    assert_eq!(
        card.plan.items[2].conflict.as_deref(),
        Some(Reason::NotPicked.text()),
        "the left-out item stays on the card, unticked, with its reason"
    );
    assert_eq!(card.plan.items[1].depends_on, vec![11]);
    match &card.plan.items[1].op {
        PlanOp::Create { slot0, items } => {
            assert_eq!(*slot0, 22);
            let NewElem::Opening { wall, .. } = &items[0] else {
                panic!("a door")
            };
            assert_eq!(wall.slot(), Some(21), "the door names the renumbered wall");
        }
        other => panic!("{other:?}"),
    }

    let mut scratch = Editor::new_in_memory().unwrap();
    compile(&mut card.plan, &mut scratch);
    let compiled_ok = |i: &PlanItem| i.id == 13 || (i.checked && i.conflict.is_none());
    assert!(card.plan.items.iter().all(compiled_ok));

    let mut live = Editor::new_in_memory().unwrap();
    let top = live.model.op_top().unwrap();
    let before = (count(&live, "wall"), count(&live, "opening"));
    let compiled = card.plan.compiled(live.model.change_head());
    exec::apply(&mut live, &compiled).unwrap();
    assert_eq!(live.model.op_top().unwrap(), top + 1, "the merge is one undo step");
    let after = (count(&live, "wall"), count(&live, "opening"));
    assert_eq!(after, (before.0 + 1, before.1 + 1));
    live.undo();
    assert_eq!(live.model.op_top().unwrap(), top);
    assert_eq!((count(&live, "wall"), count(&live, "opening")), before);
}

#[test]
fn merge_against_a_moved_live_model_is_rebased_with_a_note_or_refused() {
    let mut opt = Editor::new_in_memory().unwrap();
    let spec = ddc_model::WallSpec {
        a: (0.0, 10.0),
        b: (6.0, 10.0),
        level: opt.level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: opt.types[0].id,
        offset0: None,
    };
    opt.model.add_walls(&[spec]).unwrap();
    opt.reload().unwrap();
    let host: i64 = opt
        .model
        .sql()
        .query_row("SELECT max(id) FROM wall", [], |r| r.get(0))
        .unwrap();
    let mut st = PlanState::default();
    let d = st.reserve_slots(1, 1);
    let on_host = ElRef {
        cat: "wall".into(),
        id: IdOrNew::Id(host),
    };
    let doors = vec![door(on_host)];
    st.items.push(item(1, PlanOp::Create { slot0: d, items: doors }, vec![]));
    let w = st.reserve_slots(1, 2);
    let walls = vec![wall(&opt, 5.0)];
    st.items.push(item(2, PlanOp::Create { slot0: w, items: walls }, vec![]));
    st.next_item = 2;
    compile(&mut st, &mut opt);
    assert!(st.items.iter().all(|i| i.conflict.is_none()));

    let sets = Sets::default();
    let src = source(&st, &sets, OptionStatus::Draft);
    let moved = (5, 50);
    let strict = MergeOpts {
        pull_deps: true,
        rebase: false,
    };
    let refused = merge_card(&src, &Pick::Whole, moved, strict, (0, 0), None).err();
    assert_eq!(refused, Some(Refusal::Stale));

    let mut card =
        merge_card(&src, &Pick::Whole, moved, MergeOpts::default(), (0, 0), None).unwrap();
    assert!(card.rebased);
    assert_eq!(card.kept, vec![1, 2]);
    // the live model lost the host wall after the option base
    let mut scratch = Editor::new_in_memory().unwrap();
    compile(&mut card.plan, &mut scratch);
    settle(&mut card);
    assert_eq!(card.kept, vec![2]);
    assert_eq!(card.left_out.len(), 1);
    let (id, why) = &card.left_out[0];
    assert_eq!(*id, 1);
    let Reason::Conflict(text) = why else {
        panic!("{why:?}")
    };
    assert!(text.contains("gone"), "{text}");
    let s = summary(&card);
    assert!(s.contains("left 1:conflict") && s.contains("rebased"), "{s}");
}

#[test]
fn merge_card_item_keeps_why() {
    let mut opt = Editor::new_in_memory().unwrap();
    let mut st = option(&mut opt);
    st.items[0].why = "keeps the corridor clear".into();
    let sets = Sets::default();
    let src = source(&st, &sets, OptionStatus::Draft);
    let card = merge_card(&src, &Pick::Whole, BASE, MergeOpts::default(), (0, 0), None).unwrap();
    let first = card.plan.items.iter().find(|i| i.id == 1).unwrap();
    assert_eq!(first.why, "keeps the corridor clear");
}
