use super::*;
use crate::{LayerSpec, OpeningSpec, WallSpec};

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}

/// A 10 m wall along x and a 0.9 door at its start.
fn wall_with_door() -> (Model, i64, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type(
            "W",
            &[LayerSpec {
                material: "Brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let w = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (10.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    let d = m.add_opening(w, &door(0.2)).unwrap();
    (m, w, d)
}

fn door(along: f64) -> OpeningSpec {
    OpeningSpec {
        along,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    }
}

fn alongs(m: &Model, wall: i64, kind: &str) -> Vec<f64> {
    let mut st = m
        .sql()
        .prepare("SELECT along FROM opening WHERE wall=?1 AND kind=?2 ORDER BY along")
        .unwrap();
    st.query_map(params![wall, kind], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn layout_equal_step_fit_and_conflicts() {
    let s = layout(10.0, 1.0, 4, DistMode::Equal { margin: 0.6 }, &[]);
    let a: Vec<f64> = s.iter().map(|s| s.along).collect();
    for (x, y) in a.iter().zip([0.6, 3.2, 5.8, 8.4]) {
        near(*x, y);
    }
    assert!(s.iter().all(|s| s.conflict.is_none()));
    // the last edge sits exactly at the offset from the end
    near(a[3] + 1.0, 10.0 - 0.6);
    near(
        layout(10.0, 1.0, 1, DistMode::Equal { margin: 0.6 }, &[])[0].along,
        4.5,
    );
    let st = DistMode::Step {
        margin: 0.5,
        step: 2.0,
    };
    assert!(layout(10.0, 1.0, 5, st, &[])
        .iter()
        .all(|s| s.conflict.is_none()));
    assert_eq!(
        layout(10.0, 1.0, 6, st, &[])[5].conflict,
        Some(SlotConflict::OffWall)
    );
    assert_eq!(
        layout(10.0, 1.0, 12, DistMode::Equal { margin: 0.6 }, &[])[1].conflict,
        Some(SlotConflict::Overlap)
    );
    let s = layout(
        10.0,
        1.0,
        4,
        DistMode::Equal { margin: 0.6 },
        &[(9, 3.0, 4.0)],
    );
    assert_eq!(s[1].conflict, Some(SlotConflict::Opening(9)));
    assert_eq!(s[2].conflict, None);
    assert_eq!(fit_count(10.0, 1.0, 0.6, 2.0), 4);
    assert_eq!(fit_count(1.0, 1.0, 0.6, 2.0), 0);
}

#[test]
fn distribute_is_one_undo_and_stretch_relays_out() {
    let (mut m, w, d) = wall_with_door();
    let r = m
        .distribute(Target::Opening(d), 4, DistMode::Equal { margin: 0.6 }, true)
        .unwrap();
    assert!(r.rule.is_some() && r.skipped.is_empty());
    assert_eq!(r.members[0], Member::Opening(d));
    let step = (10.0 - 1.2 - 0.9) / 3.0;
    let a = alongs(&m, w, "door");
    assert_eq!(a.len(), 4);
    for (k, x) in a.iter().enumerate() {
        near(*x, 0.6 + step * k as f64);
    }
    m.assert_integrity();
    m.undo().unwrap();
    assert_eq!(alongs(&m, w, "door"), vec![0.2]);
    assert!(m.distribution_of(Target::Opening(d)).unwrap().is_none());

    m.distribute(Target::Opening(d), 4, DistMode::Equal { margin: 0.6 }, true)
        .unwrap();
    m.set_wall_axis(w, (0.0, 0.0), (14.0, 0.0)).unwrap();
    let step = (14.0 - 1.2 - 0.9) / 3.0;
    for (k, x) in alongs(&m, w, "door").iter().enumerate() {
        near(*x, 0.6 + step * k as f64);
    }
    // one edit is one step: undoing the extension restores both the wall and the layout
    m.undo().unwrap();
    let step = (10.0 - 1.2 - 0.9) / 3.0;
    for (k, x) in alongs(&m, w, "door").iter().enumerate() {
        near(*x, 0.6 + step * k as f64);
    }
    m.assert_integrity();
}

#[test]
fn conflicting_slots_are_skipped_and_count_edit_relays() {
    let (mut m, w, d) = wall_with_door();
    let win = m
        .add_opening(
            w,
            &OpeningSpec {
                along: 3.0,
                width: 1.0,
                sill: 0.9,
                height: 1.2,
                kind: "window".into(),
            },
        )
        .unwrap();
    let (_, _, pre) = m
        .distribution_preview(Target::Opening(d), 4, DistMode::Equal { margin: 0.6 })
        .unwrap();
    assert_eq!(pre[1].conflict, Some(SlotConflict::Opening(win)));
    let r = m
        .distribute(Target::Opening(d), 4, DistMode::Equal { margin: 0.6 }, true)
        .unwrap();
    assert_eq!(r.skipped, vec![(1, SlotConflict::Opening(win))]);
    assert_eq!(r.members[1], Member::Empty);
    assert_eq!(alongs(&m, w, "door").len(), 3);
    let rule = r.rule.unwrap();
    // 5 slots: pitch (10-1.2-0.9)/4 = 1.975 → slots 0.6, 2.575 (edge 3.475 on the window)…
    m.set_distribution(rule, 3, DistMode::Equal { margin: 1.0 })
        .unwrap();
    let a = alongs(&m, w, "door");
    assert_eq!(a.len(), 3, "{a:?}");
    near(a[0], 1.0);
    near(a[2], 10.0 - 1.0 - 0.9);
    m.undo().unwrap();
    assert_eq!(alongs(&m, w, "door").len(), 3);
    // break the link: the rule is removed, extending no longer moves the doors
    m.release_distribution(rule).unwrap();
    let before = alongs(&m, w, "door");
    m.set_wall_axis(w, (0.0, 0.0), (12.0, 0.0)).unwrap();
    assert_eq!(alongs(&m, w, "door"), before);
    m.assert_integrity();
}

#[test]
fn step_mode_conflict_rejects_edit_and_flip_keeps_order() {
    let (mut m, w, d) = wall_with_door();
    let st = DistMode::Step {
        margin: 0.5,
        step: 2.0,
    };
    let r = m.distribute(Target::Opening(d), 4, st, true).unwrap();
    let a = alongs(&m, w, "door");
    for (k, x) in a.iter().enumerate() {
        near(*x, 0.5 + 2.0 * k as f64);
    }
    // shortening to 6 m pushes the fourth door past the wall end: the whole edit is rejected
    let e = m.set_wall_axis(w, (0.0, 0.0), (6.0, 0.0)).unwrap_err();
    assert!(matches!(e, Error::Conflict(_)), "{e}");
    assert_eq!(alongs(&m, w, "door"), a);
    // flip: doors stay in place in world space, the rule follows
    m.flip_wall(w).unwrap();
    let f = alongs(&m, w, "door");
    for (x, y) in f.iter().zip(a.iter().rev()) {
        near(*x, 10.0 - y - 0.9);
    }
    let rule = m.distribution_of(Target::Opening(d)).unwrap().unwrap();
    assert_eq!(Some(rule.id), r.rule);
    m.assert_integrity();
}

#[test]
fn deleting_member_frees_slot_wall_delete_drops_rule() {
    let (mut m, w, d) = wall_with_door();
    let r = m
        .distribute(Target::Opening(d), 3, DistMode::Equal { margin: 0.6 }, true)
        .unwrap();
    let Member::Opening(mid) = r.members[1] else {
        panic!()
    };
    m.delete_opening(mid).unwrap();
    let rule = m.distribution_of(Target::Opening(d)).unwrap().unwrap();
    assert_eq!(rule.members[1], Member::Empty);
    // a new door reusing an id does not join the distribution
    let again = m.add_opening(w, &door(4.0)).unwrap();
    assert!(m.distribution_of(Target::Opening(again)).unwrap().is_none());
    m.undo().unwrap();
    m.undo().unwrap();
    assert_eq!(
        m.distribution_of(Target::Opening(d))
            .unwrap()
            .unwrap()
            .members[1],
        Member::Opening(mid)
    );
    m.delete_wall(w).unwrap();
    assert!(load_dist(m.sql()).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(load_dist(m.sql()).unwrap().len(), 1);
    m.assert_integrity();
}

#[test]
fn bad_input_and_double_distribution_rejected() {
    let (mut m, w, d) = wall_with_door();
    assert!(m
        .distribute(Target::Opening(d), 0, DistMode::Equal { margin: 0.6 }, true)
        .is_err());
    assert!(m
        .distribute(
            Target::Opening(d),
            3,
            DistMode::Step {
                margin: 0.0,
                step: 0.0
            },
            true
        )
        .is_err());
    assert!(m
        .distribute(Target::Wall(w), 3, DistMode::Equal { margin: 0.6 }, true)
        .is_err());
    m.distribute(Target::Opening(d), 3, DistMode::Equal { margin: 0.6 }, true)
        .unwrap();
    assert!(m
        .distribute(Target::Opening(d), 3, DistMode::Equal { margin: 0.6 }, true)
        .is_err());
    // an unlinked distribution has no rule
    let (mut m, w, d) = wall_with_door();
    let r = m
        .distribute(
            Target::Opening(d),
            2,
            DistMode::Equal { margin: 1.0 },
            false,
        )
        .unwrap();
    assert!(r.rule.is_none());
    m.set_wall_axis(w, (0.0, 0.0), (12.0, 0.0)).unwrap();
    let a = alongs(&m, w, "door");
    near(a[1], 10.0 - 1.0 - 0.9);
}

#[test]
fn split_drops_moved_members_and_keeps_rule() {
    let (mut m, w, d) = wall_with_door();
    let r = m
        .distribute(Target::Opening(d), 5, DistMode::Equal { margin: 0.6 }, true)
        .unwrap();
    let rule = r.rule.unwrap();
    // doors 0.6, 2.575 | 4.55, 6.525, 8.5: splitting at 4 m takes the three right ones
    let right = m.split_wall(w, 4.0).unwrap();
    assert_eq!(alongs(&m, right, "door").len(), 3);
    let a = alongs(&m, w, "door");
    assert_eq!(a.len(), 2, "{a:?}");
    near(a[0], 0.6);
    near(a[1], 4.0 - 0.6 - 0.9);
    let r = m.distribution(rule).unwrap().unwrap();
    assert_eq!((r.wall, r.members.len()), (w, 2));
    m.undo().unwrap();
    assert_eq!(alongs(&m, w, "door").len(), 5);
    m.assert_integrity();
}

#[test]
fn shrinking_keeps_live_template_from_tail() {
    let (mut m, w, d) = wall_with_door();
    m.add_opening(
        w,
        &OpeningSpec {
            along: 1.6,
            width: 0.6,
            sill: 0.9,
            height: 1.2,
            kind: "window".into(),
        },
    )
    .unwrap();
    let eq = DistMode::Equal { margin: 1.5 };
    let r = m.distribute(Target::Opening(d), 3, eq, true).unwrap();
    assert_eq!(r.members[0], Member::Empty);
    m.set_distribution(r.rule.unwrap(), 1, eq).unwrap();
    let a = alongs(&m, w, "door");
    assert_eq!(a.len(), 1, "{a:?}");
    near(a[0], (10.0 - 0.9) / 2.0);
    m.assert_integrity();
}
