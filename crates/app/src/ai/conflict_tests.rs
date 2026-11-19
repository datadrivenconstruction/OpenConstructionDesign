use super::*;
use crate::ai::plan::{replay_checked, ElRef, PlanOp, PlanState, Targets};
use crate::ai::readset::ItemSets;
use crate::ai::sets::Sets;
use crate::query::{Assign, Val};
use ddc_model::WallSpec;
use std::collections::HashMap;
use std::sync::Arc;

fn item(id: u32, reads: ReadSet, writes: ReadSet, deps: &[u32]) -> PlanItem {
    PlanItem {
        id,
        title: format!("item {id}"),
        why: String::new(),
        op: PlanOp::Delete {
            target: Targets::Refs(vec![]),
        },
        checked: true,
        depends_on: deps.to_vec(),
        actor: "ai".into(),
        conflict: None,
        sets: Arc::new(ItemSets {
            reads,
            writes,
            ..Default::default()
        }),
    }
}

fn rows(t: &'static str, ids: &[i64]) -> ReadSet {
    let mut r = ReadSet::default();
    ids.iter().for_each(|i| r.row(t, *i));
    r
}

fn changes(rows: &[(&str, &[i64])]) -> Changes {
    Changes {
        head: 1,
        all: false,
        rows: rows
            .iter()
            .map(|(t, ids)| (t.to_string(), ids.iter().copied().collect()))
            .collect(),
    }
}

/// Doors of L2 (by filter), windows of L3 (by id), columns of L1 (by id), the second built on
/// nothing, the third on the first.
fn three() -> Vec<PlanItem> {
    let mut doors = rows("opening", &[214, 215]);
    doors.table("opening");
    doors.row("level", 2);
    let windows = rows("opening", &[301, 302]);
    let mut cols = rows("col", &[7]);
    cols.row("level", 1);
    vec![
        item(1, doors, rows("opening", &[214, 215]), &[]),
        item(2, windows, rows("opening", &[301, 302]), &[]),
        item(3, cols, rows("col", &[7]), &[1]),
    ]
}

#[test]
fn unrelated_edit_keeps_plan() {
    // a wall moved on L5, its derived rows recomputed
    let ch = changes(&[("wall", &[55]), ("wall_qty", &[900, 901])]);
    assert!(conflicts(&three(), &ch, &|_, _| false).is_empty());
    assert!(conflicts(&three(), &Changes::default(), &|_, _| true).is_empty());
}

#[test]
fn deleted_target_flags_item_and_dependents() {
    let ch = changes(&[("opening", &[214]), ("wall_qty", &[3])]);
    let hit = conflicts(&three(), &ch, &|t, id| t == "opening" && id == 214);
    let ids: Vec<u32> = hit.iter().map(|h| h.0).collect();
    assert_eq!(ids, vec![1, 3], "{hit:?}");
    assert_eq!(hit[0].1.kind, ConflictKind::Deleted);
    assert_eq!(hit[0].1.rows, vec![("opening".to_string(), 214)]);
    assert_eq!(hit[1].1.via, Some(1));
    let text = hit[0].1.text();
    assert!(text.contains("plan_conflict") && text.contains("opening:214"), "{text}");
    assert!(hit[1].1.text().contains("item 1"));
    // an unticked item is not on the copy
    let mut items = three();
    items[0].checked = false;
    assert!(conflicts(&items, &ch, &|_, _| true).is_empty());
}

/// The card reads the refusal back: the person sees words and the rows, never the JSON.
#[test]
fn the_card_shows_a_human_text() {
    let ch = changes(&[("opening", &[214])]);
    let hit = conflicts(&three(), &ch, &|t, id| t == "opening" && id == 214);
    let wire = hit[0].1.text();
    assert!(wire.starts_with(ddc_ai::refusal::WIRE_PREFIX), "{wire}");
    for lang in ["en", "ru"] {
        let shown = crate::ai::ui::logic::conflict_text(&wire, lang);
        assert!(!shown.contains('{') && !shown.contains("refused"), "{shown}");
        assert!(shown.contains("deleted meanwhile: opening:214"), "{shown}");
    }
    let via = crate::ai::ui::logic::conflict_text(&hit[1].1.text(), "en");
    assert!(via.contains("depends on item 1") && !via.contains('{'), "{via}");
    // a compile error is plain text and stays so
    assert_eq!(crate::ai::ui::logic::conflict_text("element is gone", "en"), "element is gone");
}

#[test]
fn insert_into_filtered_table_flags() {
    // a new door somewhere: the filter of item 1 may now match it, the windows by id stay
    let ch = changes(&[("opening", &[999])]);
    let hit = conflicts(&three(), &ch, &|_, _| false);
    let ids: Vec<u32> = hit.iter().map(|h| h.0).collect();
    assert_eq!(ids, vec![1, 3]);
    assert_eq!(hit[0].1.kind, ConflictKind::Inserted);
    assert_eq!(hit[0].1.tables, vec!["opening".to_string()]);
    // a window it wrote, changed by the person: changed
    let hit = conflicts(&three(), &changes(&[("opening", &[302])]), &|_, _| false);
    assert_eq!(hit.iter().map(|h| h.0).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(hit[1].1.kind, ConflictKind::Changed);
    // derived rows the item wrote do not count, a coalesced element table does
    let mut w = rows("wall_qty", &[5]);
    w.row("wall", 9);
    let items = vec![item(1, ReadSet::default(), w, &[])];
    assert!(conflicts(&items, &changes(&[("wall_qty", &[5])]), &|_, _| false).is_empty());
    // the journal marks a coalesced table with i64::MIN
    let mut ch = changes(&[("wall", &[i64::MIN])]);
    ch.rows.get_mut("wall").unwrap().insert(1);
    assert_eq!(conflicts(&items, &ch, &|_, _| false).len(), 1);
}

#[test]
fn a_table_written_whole_is_changed_not_inserted() {
    let whole = || changes(&[("wall", &[i64::MIN])]);
    // rows it wrote, rows it read, the table it filtered: every case reads as changed
    let wrote = vec![item(1, ReadSet::default(), rows("wall", &[9]), &[])];
    let read = vec![item(1, rows("wall", &[9]), ReadSet::default(), &[])];
    let mut t = ReadSet::default();
    t.table("wall");
    let filtered = vec![item(1, t, ReadSet::default(), &[])];
    for items in [wrote, read, filtered] {
        let hit = conflicts(&items, &whole(), &|_, _| false);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].1.kind, ConflictKind::Changed, "{:?}", hit[0].1);
        assert!(hit[0].1.text().contains("changed meanwhile"), "{}", hit[0].1.text());
    }
}

#[test]
fn meta_change_all_stale() {
    let mut ch = changes(&[]);
    ch.all = true;
    let hit = conflicts(&three(), &ch, &|_, _| false);
    assert_eq!(hit.len(), 3);
    assert!(hit.iter().all(|h| h.1.all && h.1.via.is_none()));
    assert!(hit[0].1.text().contains("project settings"));
    // the live side: a project setting written reads as "everything"
    let mut ed = crate::editor::Editor::new_in_memory().unwrap();
    let h = ed.model.change_head();
    let lv = ed.level;
    ed.model.sql().execute("UPDATE level SET elevation=1 WHERE id=?1", [lv]).unwrap();
    let ch = ed.model.plan_changes(h);
    assert!(!ch.all && ch.ids("level").any(|i| i == lv), "{ch:?}");
    ed.model
        .sql()
        .execute("INSERT OR REPLACE INTO meta(key, value) VALUES('plan_probe', 1)", [])
        .unwrap();
    assert!(ed.model.plan_changes(h).all);
}

/// The budget: 10 000 items against 1 000 changed rows in ≤ 20 ms.
#[test]
fn conflict_check_budget() {
    let items: Vec<PlanItem> = (0..10_000u32)
        .map(|i| {
            let ids: Vec<i64> = (0..3).map(|k| i64::from(i) * 3 + k).collect();
            let mut r = rows("wall", &ids);
            r.row("level", i64::from(i % 40));
            item(i + 1, r, rows("wall", &ids), if i % 7 == 6 { &[1] } else { &[] })
        })
        .collect();
    let changed: Vec<i64> = (0..1_000i64).map(|k| (k * 7_919) % 30_000).collect();
    let ch = changes(&[("wall", &changed), ("wall_qty", &changed)]);
    let mut best = std::time::Duration::MAX;
    let mut n = 0;
    for _ in 0..3 {
        let t = std::time::Instant::now();
        n = conflicts(&items, &ch, &|_, _| false).len();
        best = best.min(t.elapsed());
    }
    assert!(n >= 900, "{n}");
    assert!(best <= std::time::Duration::from_millis(20), "{best:?}");
}

/// Walls one per metre along y, the plan raises the first `n` by an item each.
fn wall_plan(walls: usize, n: usize, val: Val) -> (crate::editor::Editor, PlanState, Vec<i64>) {
    let mut ed = crate::editor::Editor::new_in_memory().unwrap();
    let specs: Vec<WallSpec> = (0..walls)
        .map(|i| WallSpec {
            a: (0.0, 2.0 * i as f64),
            b: (5.0, 2.0 * i as f64),
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ed.types[0].id,
            offset0: None,
        })
        .collect();
    let ids = ed.model.add_walls(&specs).unwrap();
    ed.reload().unwrap();
    let mut st = PlanState::default();
    for (i, w) in ids.iter().take(n).enumerate() {
        let id = i as u32 + 1;
        let mut it = item(id, ReadSet::default(), ReadSet::default(), &[]);
        it.op = PlanOp::Assign {
            target: Targets::Refs(vec![ElRef::parse(&format!("wall:{w}")).unwrap()]),
            set: vec![Assign {
                key: "height".into(),
                val: val.clone(),
            }],
        };
        st.items.push(it);
        st.next_item = id;
    }
    (ed, st, ids)
}

fn fps(st: &PlanState) -> HashMap<u32, u64> {
    st.items.iter().map(|i| (i.id, i.sets.fp)).collect()
}

/// Random edits outside every set: no item is hit and every item leaves its rows as before.
#[test]
fn conflict_free_items_replay_same() {
    let (mut ed, mut st, walls) = wall_plan(40, 12, Val::Rel('+', "100".into()));
    let top = ed.model.op_top().unwrap();
    replay_checked(&mut st, &mut ed, &Sets::default(), &[], &[], 1).unwrap();
    let before = fps(&st);
    assert!(before.values().all(|f| *f != 0));
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    for round in 0..8 {
        ed.model.rollback_to(top).unwrap();
        ed.reload().unwrap();
        let head = ed.model.change_head();
        for _ in 0..5 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let w = walls[12 + (seed >> 33) as usize % (walls.len() - 12)];
            let dx = ((seed >> 20) % 300) as f64 / 100.0;
            ed.model
                .sql()
                .execute(
                    "UPDATE wall SET ax=ax+?1, bx=bx+?1, height=height+?2 WHERE id=?3",
                    rusqlite::params![dx, dx / 10.0, w],
                )
                .unwrap();
        }
        ed.reload().unwrap();
        let ch = ed.model.changes_since(head);
        let hit = conflicts(&st.items, &ch, &|t, id| row_gone(ed.model.sql(), t, id));
        assert!(hit.is_empty(), "round {round}: {hit:?}");
        replay_checked(&mut st, &mut ed, &Sets::default(), &[], &[], 2 + round).unwrap();
        assert!(st.items.iter().all(|i| i.conflict.is_none()), "round {round}");
        assert_eq!(fps(&st), before, "round {round}");
        assert!(st.items.iter().all(|i| i.sets.base == 2 + round));
    }
    // an edit inside a set: the check flags it, and so would the replay on its own
    ed.model.rollback_to(top).unwrap();
    ed.reload().unwrap();
    let head = ed.model.change_head();
    ed.model
        .sql()
        .execute("UPDATE wall SET height=4 WHERE id=?1", [walls[3]])
        .unwrap();
    ed.reload().unwrap();
    let hit = conflicts(&st.items, &ed.model.changes_since(head), &|_, _| false);
    assert_eq!(hit.iter().map(|h| h.0).collect::<Vec<_>>(), vec![4]);
    replay_checked(&mut st, &mut ed, &Sets::default(), &[], &[], 99).unwrap();
    let flagged: Vec<u32> = st.items.iter().filter(|i| i.conflict.is_some()).map(|i| i.id).collect();
    assert_eq!(flagged, vec![4], "the result of item 4 moved");
    assert!(st.held.contains_key(&4));
}
