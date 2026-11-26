// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::editor::{Editor, Sel};
use crate::modify_tools::{ModTool, Op, Out};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn near(a: V2, b: V2) -> bool {
    (a - b).len() < 1e-6
}

fn ends(ed: &Editor, id: i64) -> (V2, V2) {
    let w = ed.walls.iter().find(|w| w.id == id).expect("wall");
    (w.a, w.b)
}

/// One wall (0,0)-(4,0) and a move tool with its base point at the origin, cursor at (1,2).
fn moving() -> (Editor, i64, ModTool) {
    let mut ed = Editor::new_in_memory().unwrap();
    crate::modify_tools::tests::walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
    let id = ed.walls[0].id;
    ed.selection = vec![Sel::Wall(id)];
    let mut t = ModTool::new(Op::Move, &ed.selection);
    assert_eq!(t.click(&mut ed, v2(0.0, 0.0)), Out::Nothing);
    t.hover(&ed, v2(1.0, 2.0));
    (ed, id, t)
}

fn counting(c: &Arc<AtomicUsize>, key: u64) -> Ask {
    let c = c.clone();
    Ask {
        key,
        field: Field::Point,
        op: OpFn(Arc::new(move |_m: &mut Model| {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })),
        size: 1,
    }
}

/// `a` made slower than `BUDGET`, with the same key and result.
fn slow(a: &Ask) -> Ask {
    let op = a.op.0.clone();
    Ask {
        op: OpFn(Arc::new(move |m: &mut Model| {
            std::thread::sleep(std::time::Duration::from_millis(40));
            op(m)
        })),
        ..a.clone()
    }
}

#[test]
fn trial_never_mutates_model() {
    let (mut ed, id, mut t) = moving();
    // checks results, not timing: a loaded test run must not turn the ghost off
    t.trial.slack = 60.0;
    let (head, steps) = (ed.model.change_head(), ed.undo_len());
    assert_eq!(t.pulse(&mut ed, 0.0, 1), Some(SETTLE));
    t.pulse(&mut ed, 0.3, 2);
    assert_eq!(t.trial.runs, 1);
    let g = t.trial.ghost(ed.model.serial(), 2).expect("ghost").to_vec();
    assert!(
        g.iter().any(|s| s.len() == 2 && near(s[0], v2(1.0, 2.0)) && near(s[1], v2(5.0, 2.0))),
        "{g:?}"
    );
    // the model, the journal and the loaded plan are as before
    assert_eq!(ed.model.change_head(), head);
    assert_eq!(ed.undo_len(), steps);
    assert!(near(ends(&ed, id).0, v2(0.0, 0.0)));
    ed.reload().unwrap();
    assert!(near(ends(&ed, id).0, v2(0.0, 0.0)), "the file row is unchanged");
    // Esc leaves nothing behind
    assert_eq!(t.escape(), Out::Nothing);
    t.pulse(&mut ed, 0.4, 3);
    assert!(t.trial.ghost(ed.model.serial(), 3).is_none());
    assert_eq!(ed.model.change_head(), head);
    assert_eq!(ed.undo_len(), steps);
    // the commit after a trial is exactly one undo step
    assert_eq!(t.click(&mut ed, v2(0.0, 0.0)), Out::Nothing);
    t.hover(&ed, v2(1.0, 2.0));
    t.pulse(&mut ed, 1.0, 4);
    t.pulse(&mut ed, 1.3, 5);
    assert_eq!(t.click(&mut ed, v2(1.0, 2.0)), Out::Exit);
    assert_eq!(ed.undo_len(), steps + 1);
    assert!(near(ends(&ed, id).0, v2(1.0, 2.0)));
    ed.undo();
    assert!(near(ends(&ed, id).0, v2(0.0, 0.0)));
}

#[test]
fn trial_debounced_and_cancelled_by_new_input() {
    let mut m = Model::open(":memory:").unwrap();
    let doc = m.serial();
    let (c1, c2, c3) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let (a1, a2, a3) = (counting(&c1, 1), counting(&c2, 2), counting(&c3, 3));
    let mut t = Trial::default();
    assert!(!t.pulse(&mut m, 0, Some(&a1), 0.0, 1));
    // new input before the first settled: the first never runs, the wait restarts
    assert!(!t.pulse(&mut m, 0, Some(&a2), 0.2, 2));
    assert!(!t.pulse(&mut m, 0, Some(&a2), 0.4, 3));
    assert!((t.wait(0.4).unwrap() - 0.05).abs() < 1e-9);
    assert!(t.pulse(&mut m, 0, Some(&a2), 0.46, 4));
    assert!(!t.pulse(&mut m, 0, Some(&a2), 0.6, 5), "one trial per input");
    assert_eq!(t.wait(0.6), None);
    assert_eq!(
        (c1.load(Ordering::SeqCst), c2.load(Ordering::SeqCst), t.runs),
        (0, 1, 1)
    );
    assert_eq!(t.verdict(2, doc), Some(&Verdict::Ghost(vec![])));
    // newer input drops the finished result of the old one
    assert!(!t.pulse(&mut m, 0, Some(&a3), 0.7, 6));
    assert_eq!(t.verdict(2, doc), None);
    assert!(t.ghost(doc, 6).is_none());
    // no input: nothing pending
    t.pulse(&mut m, 0, None, 0.8, 7);
    assert_eq!(t.wait(0.8), None);
    assert_eq!(c3.load(Ordering::SeqCst), 0);
}

#[test]
fn refusal_blocks_enter_with_field_reason() {
    let mut ed = Editor::new_in_memory().unwrap();
    crate::modify_tools::tests::walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
    let mut t = ModTool::new(Op::Offset, &[]);
    t.opts.dist = 0.0;
    t.trial.slack = 60.0;
    let at = v2(2.0, 0.05);
    t.hover(&ed, at);
    t.pulse(&mut ed, 0.0, 1);
    t.pulse(&mut ed, 0.3, 2);
    let doc = ed.model.serial();
    let (field, why) = t.trial.refusal(doc).expect("refused before commit");
    assert_eq!(field, Field::Distance);
    let want = refusal_text(field, why);
    assert!(t.trial.ghost(doc, 2).is_none(), "a refused edit has no ghost");
    let (head, steps, n) = (ed.model.change_head(), ed.undo_len(), ed.walls.len());
    assert_eq!(t.click(&mut ed, at), Out::Error(want));
    assert_eq!(ed.model.change_head(), head);
    assert_eq!((ed.undo_len(), ed.walls.len()), (steps, n));
    // the refusal stays on its field until the input changes
    assert!(t.trial.refusal(doc).is_some());
    t.opts.dist = 0.5;
    t.pulse(&mut ed, 1.0, 3);
    assert!(t.trial.refusal(doc).is_none());
    t.pulse(&mut ed, 1.3, 4);
    assert!(t.trial.ghost(doc, 4).is_some_and(|g| !g.is_empty()));
    assert_eq!(t.click(&mut ed, at), Out::Done);
    assert_eq!((ed.undo_len(), ed.walls.len()), (steps + 1, n + 1));
}

#[test]
fn stale_preview_not_drawn_after_doc_switch() {
    let (mut ed, _, mut t) = moving();
    t.pulse(&mut ed, 0.0, 10);
    t.pulse(&mut ed, 0.3, 11);
    let doc = ed.model.serial();
    let key = t.ask(&ed).expect("ask").key;
    assert!(t.trial.ghost(doc, 11).is_some());
    assert!(t.trial.ghost(doc, 12).is_some(), "asked last frame");
    assert!(t.trial.ghost(doc, 13).is_none(), "not asked for two frames");
    let other = Editor::new_in_memory().unwrap();
    assert_ne!(other.model.serial(), doc);
    assert!(t.trial.ghost(other.model.serial(), 11).is_none());
    assert!(t.trial.verdict(key, other.model.serial()).is_none());
    assert!(t.trial.blocks(key, other.model.serial()).is_none());
}

#[test]
fn trial_over_budget_turns_ghost_off() {
    let mut m = Model::open(":memory:").unwrap();
    let doc = m.serial();
    let (c1, c2) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut a1 = slow(&counting(&c1, 1));
    a1.size = 5;
    let mut t = Trial::default();
    t.pulse(&mut m, 0, Some(&a1), 0.0, 1);
    assert!(t.pulse(&mut m, 0, Some(&a1), 0.3, 2), "the slow trial itself runs");
    assert_eq!(c1.load(Ordering::SeqCst), 1);
    assert!(t.is_off());
    assert!(t.off_hint().is_some());
    assert!(t.ghost(doc, 2).is_none(), "plain outline only");
    assert_eq!(t.wait(0.3), None, "no repaint waits for a trial");
    // the same selection size with new input: no more trials this session
    let mut a2 = counting(&c2, 2);
    a2.size = 5;
    for (i, now) in [0.4, 0.8, 1.2].into_iter().enumerate() {
        assert!(!t.pulse(&mut m, 0, Some(&a2), now, 3 + i as u64));
    }
    assert_eq!((c2.load(Ordering::SeqCst), t.runs), (0, 1));
    assert!(t.ghost(doc, 5).is_none());
    assert!(t.is_off());
}

#[test]
fn ghost_off_commit_still_refuses() {
    let mut ed = Editor::new_in_memory().unwrap();
    crate::modify_tools::tests::walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
    let mut t = ModTool::new(Op::Offset, &[]);
    t.opts.dist = 0.0;
    let at = v2(2.0, 0.05);
    t.hover(&ed, at);
    // the trial of exactly this input goes over budget: trials turn off, its refusal stays
    let a = slow(&t.ask(&ed).expect("ask"));
    let lv = ed.level;
    t.trial.pulse(&mut ed.model, lv, Some(&a), 0.0, 1);
    assert!(t.trial.pulse(&mut ed.model, lv, Some(&a), 0.3, 2));
    assert!(t.trial.is_off());
    let doc = ed.model.serial();
    assert!(t.trial.ghost(doc, 2).is_none());
    let (field, why) = t.trial.refusal(doc).expect("refusal kept");
    let want = refusal_text(field, why);
    let (head, steps, n) = (ed.model.change_head(), ed.undo_len(), ed.walls.len());
    assert_eq!(t.click(&mut ed, at), Out::Error(want), "refused exactly as before");
    assert_eq!(ed.model.change_head(), head);
    assert_eq!((ed.undo_len(), ed.walls.len()), (steps, n));
    // new input, no trial: the commit itself still refuses and changes nothing
    let other = v2(2.0, -0.05);
    t.hover(&ed, other);
    assert_ne!(t.ask(&ed).expect("ask").key, a.key);
    t.pulse(&mut ed, 1.0, 3);
    t.pulse(&mut ed, 1.3, 4);
    assert_eq!(t.trial.runs, 1, "trials stay off");
    assert!(matches!(t.click(&mut ed, other), Out::Error(_)));
    assert_eq!(ed.model.change_head(), head);
    assert_eq!((ed.undo_len(), ed.walls.len()), (steps, n));
}

#[test]
fn budget_resets_on_new_tool() {
    let (mut ed, _, mut t) = moving();
    let a = slow(&t.ask(&ed).expect("ask"));
    let lv = ed.level;
    t.trial.pulse(&mut ed.model, lv, Some(&a), 0.0, 1);
    t.trial.pulse(&mut ed.model, lv, Some(&a), 0.3, 2);
    assert!(t.trial.is_off());
    t.hover(&ed, v2(2.0, 3.0));
    t.pulse(&mut ed, 1.0, 3);
    t.pulse(&mut ed, 1.3, 4);
    assert_eq!(t.trial.runs, 1);
    assert!(t.trial.ghost(ed.model.serial(), 4).is_none());
    // a new tool session previews again
    let mut t2 = ModTool::new(Op::Move, &ed.selection);
    assert!(!t2.trial.is_off());
    assert_eq!(t2.click(&mut ed, v2(0.0, 0.0)), Out::Nothing);
    t2.hover(&ed, v2(1.0, 2.0));
    t2.pulse(&mut ed, 2.0, 5);
    t2.pulse(&mut ed, 2.3, 6);
    assert_eq!(t2.trial.runs, 1);
    assert!(!t2.trial.is_off());
    assert!(t2.trial.ghost(ed.model.serial(), 6).is_some_and(|g| !g.is_empty()));
    assert!(t2.trial.off_hint().is_none());
}

#[test]
fn budget_resets_on_smaller_selection() {
    let mut m = Model::open(":memory:").unwrap();
    let doc = m.serial();
    let c = Arc::new(AtomicUsize::new(0));
    let mut big = slow(&counting(&c, 1));
    big.size = 3;
    let mut t = Trial::default();
    t.pulse(&mut m, 0, Some(&big), 0.0, 1);
    t.pulse(&mut m, 0, Some(&big), 0.3, 2);
    assert!(t.is_off());
    let mut same = counting(&c, 2);
    same.size = 3;
    t.pulse(&mut m, 0, Some(&same), 0.4, 3);
    assert!(!t.pulse(&mut m, 0, Some(&same), 0.8, 4));
    let mut small = counting(&c, 3);
    small.size = 2;
    t.pulse(&mut m, 0, Some(&small), 1.0, 5);
    assert!(!t.is_off(), "a smaller selection turns trials on");
    assert!(t.pulse(&mut m, 0, Some(&small), 1.3, 6));
    assert_eq!((c.load(Ordering::SeqCst), t.runs), (2, 2));
    assert_eq!(t.ghost(doc, 6), Some(&[][..]));
}

#[test]
#[ignore]
fn trial_cost_10k_walls() {
    let mut ed = Editor::new_in_memory().unwrap();
    let segs: Vec<_> = (0..10_000)
        .map(|i| {
            let (x, y) = ((i % 100) as f64 * 5.0, (i / 100) as f64 * 5.0);
            ((x, y), (x + 4.0, y))
        })
        .collect();
    crate::modify_tools::tests::walls(&mut ed, &segs);
    ed.selection = ed.walls.iter().map(|w| Sel::Wall(w.id)).collect();
    let mut t = ModTool::new(Op::Move, &ed.selection);
    assert_eq!(t.click(&mut ed, v2(0.0, 0.0)), Out::Nothing);
    t.hover(&ed, v2(1.0, 2.0));
    let a = t.ask(&ed).expect("ask");
    let lv = ed.level;
    let t0 = std::time::Instant::now();
    let v = run(&mut ed.model, lv, &a);
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    println!("trial_cost_10k_walls: {ms:.1} ms (budget {:.0} ms)", BUDGET * 1e3);
    assert!(matches!(v, Verdict::Ghost(_)));
}
