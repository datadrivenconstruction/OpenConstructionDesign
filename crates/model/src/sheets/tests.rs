use super::*;
use crate::{SectionSpec, ViewKind};

fn section() -> SectionSpec {
    SectionSpec {
        kind: ViewKind::Section,
        name: "1-1".into(),
        a: (1.0, -1.0),
        b: (1.0, 5.0),
        depth: 10.0,
        far_clip: false,
    }
}

fn vp(sheet: i64, view: ViewRef) -> ViewportDef {
    ViewportDef {
        sheet,
        view,
        scale: 100.0,
        center: (180.0, 150.0),
        crop: None,
        title: "Plan".into(),
    }
}

fn count(m: &Model, t: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn numbers_increment_keeping_width() {
    assert_eq!(increment_number("A-101"), "A-102");
    assert_eq!(increment_number("A-109"), "A-110");
    assert_eq!(
        increment_number(crate::test_ru::ru("sheet_no_09")),
        crate::test_ru::ru("sheet_no_10")
    );
    assert_eq!(increment_number("A-1.a"), "A-2.a");
    assert_eq!(
        increment_number(crate::test_ru::ru("word_sheet")),
        crate::test_ru::ru("word_sheet_1")
    );
    assert_eq!(increment_number("99"), "100");
    assert_eq!(
        increment_number("A-18446744073709551615"),
        "A-18446744073709551616"
    );
    assert_eq!(
        increment_number("X-099999999999999999999"),
        "X-100000000000000000000"
    );
}

/// Moving and deleting a mixed selection, properties with revisions: each is one undo step.
#[test]
fn sheet_items_are_single_undo_steps() {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let plan = ViewRef::Plan {
        level: l1,
        cut: 1.2,
    };
    let s = m.add_sheet(&SheetDef::new("A-101", "a")).unwrap();
    let v = m.add_viewport(&vp(s, plan)).unwrap();
    let place = |pos| SchedulePlace {
        sheet: s,
        schedule: "doors".into(),
        pos,
    };
    let q = m.add_schedule_place(&place((10.0, 10.0))).unwrap();
    m.move_sheet_items(&[(v, (200.0, 160.0))], &[(q, (20.0, 30.0))])
        .unwrap();
    assert_eq!(m.viewport(v).unwrap().center, (200.0, 160.0));
    assert_eq!(m.schedule_places(s).unwrap()[0].1.pos, (20.0, 30.0));
    m.undo().unwrap();
    assert_eq!(m.viewport(v).unwrap().center, (180.0, 150.0));
    assert_eq!(m.schedule_places(s).unwrap()[0].1.pos, (10.0, 10.0));
    // an error on the second item also rolls back the first
    assert!(m
        .move_sheet_items(&[(v, (1.0, 1.0))], &[(999, (0.0, 0.0))])
        .is_err());
    assert_eq!(m.viewport(v).unwrap().center, (180.0, 150.0));

    m.delete_sheet_items(&[v], &[q]).unwrap();
    assert_eq!((count(&m, "viewport"), count(&m, "sheet_schedule")), (0, 0));
    m.undo().unwrap();
    assert_eq!((count(&m, "viewport"), count(&m, "sheet_schedule")), (1, 1));

    let rev = Revision {
        mark: "1".into(),
        date: "2026-11-04".into(),
        description: "d".into(),
        author: "a".into(),
    };
    let mut d = m.sheet(s).unwrap();
    d.name = "b".into();
    m.save_sheet_with_revisions(Some(s), &d, std::slice::from_ref(&rev))
        .unwrap();
    assert_eq!(
        (m.sheet(s).unwrap().name, m.revisions(s).unwrap().len()),
        ("b".to_string(), 1)
    );
    m.undo().unwrap();
    assert_eq!(
        (m.sheet(s).unwrap().name, m.revisions(s).unwrap().len()),
        ("a".to_string(), 0)
    );
    let n = m
        .save_sheet_with_revisions(None, &SheetDef::new("A-102", "c"), &[rev])
        .unwrap();
    assert_eq!(m.revisions(n).unwrap().len(), 1);
    m.undo().unwrap();
    assert!(m.sheet(n).is_err());
    assert_eq!(count(&m, "sheet_rev"), 0);
}

#[test]
fn sheet_crud_undo_and_unique_numbers() {
    let mut m = Model::open(":memory:").unwrap();
    assert_eq!(m.next_sheet_number().unwrap(), "A-101");
    let mut s = SheetDef::new("A-101", "Plan of floor 1");
    s.fields.insert("developer".into(), "Ivanov".into());
    let a = m.add_sheet(&s).unwrap();
    assert_eq!(m.sheet(a).unwrap(), s);
    assert_eq!(m.next_sheet_number().unwrap(), "A-102");
    assert!(m.add_sheet(&SheetDef::new("A-101", "dup")).is_err());
    let mut bad = SheetDef::new("X", "x");
    bad.paper = "B5".into();
    assert!(m.add_sheet(&bad).is_err());
    bad.paper = "ANSI D".into();
    bad.standard = "din".into();
    assert!(m.add_sheet(&bad).is_err());

    let mut s2 = s.clone();
    s2.name = "Plan".into();
    s2.paper = "A1".into();
    s2.landscape = false;
    m.update_sheet(a, &s2).unwrap();
    assert_eq!(m.sheet(a).unwrap(), s2);
    m.undo().unwrap();
    assert_eq!(m.sheet(a).unwrap(), s);

    let b = m.add_sheet(&SheetDef::new("A-102", "Sections")).unwrap();
    let mut clash = m.sheet(b).unwrap();
    clash.number = "A-101".into();
    assert!(m.update_sheet(b, &clash).is_err());
    m.delete_sheet(a).unwrap();
    assert!(m.sheet(a).is_err());
    m.undo().unwrap();
    assert_eq!(m.sheet(a).unwrap(), s);
    m.assert_integrity();
}

#[test]
fn order_and_renumbering() {
    let mut m = Model::open(":memory:").unwrap();
    let ids: Vec<i64> = ["A-101", "A-102", "A-103"]
        .iter()
        .map(|n| m.add_sheet(&SheetDef::new(n, n)).unwrap())
        .collect();
    m.move_sheet(ids[2], -2).unwrap();
    let order: Vec<i64> = m.sheets().unwrap().iter().map(|s| s.0).collect();
    assert_eq!(order, vec![ids[2], ids[0], ids[1]]);
    assert!(m.reorder_sheets(&[ids[0], ids[1]]).is_err());
    // swapping numbers within a set does not hit uniqueness
    m.renumber_sheets(&order, "A-101").unwrap();
    let nums: Vec<String> = m
        .sheets()
        .unwrap()
        .into_iter()
        .map(|s| s.1.number)
        .collect();
    assert_eq!(nums, ["A-101", "A-102", "A-103"]);
    assert_eq!(m.sheet(ids[2]).unwrap().number, "A-101");
    // a clash with a sheet outside the set is rejected without changes
    assert!(m.renumber_sheets(&[ids[2]], "A-102").is_err());
    assert_eq!(m.sheet(ids[2]).unwrap().number, "A-101");
    m.undo().unwrap();
    assert_eq!(m.sheet(ids[2]).unwrap().number, "A-103");
    m.undo().unwrap();
    let order: Vec<i64> = m.sheets().unwrap().iter().map(|s| s.0).collect();
    assert_eq!(order, ids);
}

#[test]
fn revisions_replace_and_undo() {
    let mut m = Model::open(":memory:").unwrap();
    let a = m.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
    let r1 = Revision {
        mark: "1".into(),
        date: "01.11.26".into(),
        description: "Issue".into(),
        author: "AI".into(),
    };
    m.set_revisions(a, std::slice::from_ref(&r1)).unwrap();
    let r2 = Revision {
        mark: "2".into(),
        description: "Comments".into(),
        ..r1.clone()
    };
    m.set_revisions(a, &[r1.clone(), r2.clone()]).unwrap();
    assert_eq!(m.revisions(a).unwrap(), vec![r1.clone(), r2]);
    m.undo().unwrap();
    assert_eq!(m.revisions(a).unwrap(), vec![r1]);
    m.undo().unwrap();
    assert!(m.revisions(a).unwrap().is_empty());
}

#[test]
fn viewports_validate_move_and_undo() {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let s = m.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
    let plan = ViewRef::Plan {
        level: l1,
        cut: DEFAULT_CUT,
    };
    let v = m.add_viewport(&vp(s, plan)).unwrap();
    assert_eq!(m.viewport(v).unwrap(), vp(s, plan));
    assert!(m.add_viewport(&vp(999, plan)).is_err());
    assert!(m
        .add_viewport(&vp(
            s,
            ViewRef::Plan {
                level: 77,
                cut: 1.2
            }
        ))
        .is_err());
    assert!(m.add_viewport(&vp(s, ViewRef::View(5))).is_err());
    let mut bad = vp(s, plan);
    bad.scale = 0.0;
    assert!(m.add_viewport(&bad).is_err());
    bad.scale = 50.0;
    bad.crop = Some([1.0, 0.0, 1.0, 3.0]);
    assert!(m.add_viewport(&bad).is_err());

    let mut moved = vp(s, plan);
    moved.scale = 50.0;
    moved.crop = Some([0.0, 0.0, 6.0, 4.0]);
    m.update_viewport(v, &moved).unwrap();
    assert_eq!(m.viewport(v).unwrap(), moved);
    m.move_viewports(&[(v, (100.0, 120.0))]).unwrap();
    assert_eq!(m.viewport(v).unwrap().center, (100.0, 120.0));
    m.undo().unwrap();
    assert_eq!(m.viewport(v).unwrap(), moved);
    m.undo().unwrap();
    assert_eq!(m.viewport(v).unwrap(), vp(s, plan));
    assert_eq!(m.placements(plan).unwrap(), vec![(v, s)]);
    m.delete_viewport(v).unwrap();
    assert!(m.viewports(s).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.viewports(s).unwrap().len(), 1);
    m.assert_integrity();
}

#[test]
fn deleting_a_view_removes_its_viewports_and_undo_restores() {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let sec = m.add_view_def(&section()).unwrap();
    let s = m.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
    let vs = m.add_viewport(&vp(s, ViewRef::View(sec))).unwrap();
    let vp2 = m
        .add_viewport(&vp(
            s,
            ViewRef::Plan {
                level: l2,
                cut: 1.2,
            },
        ))
        .unwrap();
    let keep = m
        .add_viewport(&vp(
            s,
            ViewRef::Plan {
                level: l1,
                cut: 1.2,
            },
        ))
        .unwrap();
    let before = m.viewports(s).unwrap();

    m.delete_view_def(sec).unwrap();
    let left: Vec<i64> = m.viewports(s).unwrap().iter().map(|v| v.0).collect();
    assert_eq!(left, vec![vp2, keep]);
    m.assert_integrity();
    m.undo().unwrap();
    assert_eq!(m.viewports(s).unwrap(), before);
    assert_eq!(m.viewport(vs).unwrap().view, ViewRef::View(sec));

    // an empty level is deleted together with its plan on the sheet
    m.delete_level(l2).unwrap();
    let left: Vec<i64> = m.viewports(s).unwrap().iter().map(|v| v.0).collect();
    assert_eq!(left, vec![vs, keep]);
    m.assert_integrity();
    m.undo().unwrap();
    assert_eq!(m.viewports(s).unwrap(), before);
    m.assert_integrity();
}

#[test]
fn deleting_a_sheet_cascades_and_undo_restores() {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let a = m.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
    let b = m.add_sheet(&SheetDef::new("A-102", "Schedules")).unwrap();
    m.add_viewport(&vp(
        a,
        ViewRef::Plan {
            level: l1,
            cut: 1.2,
        },
    ))
    .unwrap();
    let p = m
        .add_schedule_place(&SchedulePlace {
            sheet: a,
            schedule: "doors".into(),
            pos: (20.0, 280.0),
        })
        .unwrap();
    m.move_schedule_place(p, (30.0, 270.0)).unwrap();
    assert_eq!(m.schedule_places(a).unwrap()[0].1.pos, (30.0, 270.0));
    m.set_revisions(
        a,
        &[Revision {
            mark: "A".into(),
            ..Default::default()
        }],
    )
    .unwrap();
    let set = m.add_sheet_set("Issue 1", &[a, b]).unwrap();
    assert!(m.add_sheet_set("Issue 1", &[b]).is_err());
    assert!(m.add_sheet_set("Empty", &[404]).is_err());
    let snap: Vec<i64> = OWNED.iter().map(|t| count(&m, t)).collect();

    m.delete_sheet(a).unwrap();
    assert_eq!(
        m.sheet_sets().unwrap(),
        vec![(set, "Issue 1".into(), vec![b])]
    );
    for t in OWNED.iter().take(3) {
        assert_eq!(count(&m, t), 0, "{t}");
    }
    m.assert_integrity();
    m.undo().unwrap();
    let back: Vec<i64> = OWNED.iter().map(|t| count(&m, t)).collect();
    assert_eq!(back, snap);
    assert_eq!(
        m.sheet_sets().unwrap(),
        vec![(set, "Issue 1".into(), vec![a, b])]
    );

    m.update_sheet_set(set, "Issue 2", &[b]).unwrap();
    assert_eq!(
        m.sheet_sets().unwrap(),
        vec![(set, "Issue 2".into(), vec![b])]
    );
    m.undo().unwrap();
    assert_eq!(
        m.sheet_sets().unwrap(),
        vec![(set, "Issue 1".into(), vec![a, b])]
    );
    m.delete_sheet_set(set).unwrap();
    assert!(m.sheet_sets().unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.sheet_sets().unwrap().len(), 1);
    m.delete_schedule_place(p).unwrap();
    assert!(m.schedule_places(a).unwrap().is_empty());
    m.assert_integrity();
}

#[test]
fn plan_sheets_for_levels_are_one_undo_step() {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    m.add_sheet(&SheetDef::new("A-102", "taken")).unwrap();
    let tpl = SheetDef::new("A-101", "Plan");
    let made = m
        .add_plan_sheets(&tpl, &[l1, l2], 100.0, (200.0, 160.0))
        .unwrap();
    assert_eq!(made.len(), 2);
    let a = m.sheet(made[0]).unwrap();
    let b = m.sheet(made[1]).unwrap();
    assert_eq!((a.number.as_str(), a.name.as_str()), ("A-101", "Plan · L1"));
    assert_eq!(b.number, "A-103");
    let va = &m.viewports(made[0]).unwrap()[0].1;
    let vb = &m.viewports(made[1]).unwrap()[0].1;
    assert_eq!(
        (va.center, va.scale, va.title.as_str()),
        ((200.0, 160.0), 100.0, "L1")
    );
    assert_eq!(va.center, vb.center);
    m.undo().unwrap();
    assert_eq!(m.sheets().unwrap().len(), 1);
    assert_eq!(count(&m, "viewport"), 0);
    assert!(m
        .add_plan_sheets(&tpl, &[l1, 999], 100.0, (0.0, 0.0))
        .is_err());
    assert_eq!(m.sheets().unwrap().len(), 1);
}

#[test]
fn integrity_reports_dangling_viewports() {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let s = m.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
    let v = m
        .add_viewport(&vp(
            s,
            ViewRef::Plan {
                level: l1,
                cut: 1.2,
            },
        ))
        .unwrap();
    m.sql()
        .execute(
            "UPDATE viewport SET level=NULL, view_def=42 WHERE id=?1",
            [v],
        )
        .unwrap();
    let probs: Vec<String> = m
        .check_integrity()
        .unwrap()
        .into_iter()
        .filter(|i| i.table == "viewport")
        .map(|i| i.problem)
        .collect();
    assert_eq!(probs, vec!["view_def -> missing view_def 42".to_string()]);
    m.sql()
        .execute("UPDATE viewport SET level=?2 WHERE id=?1", [v, l1])
        .unwrap();
    let probs = m.check_integrity().unwrap();
    assert!(probs
        .iter()
        .any(|i| i.problem == "viewport must show exactly one view"));
}

/// Migration: a file of any version below 21 (including the neighbouring versions 18-20) without sheet tables gets them.
#[test]
fn older_files_gain_sheet_tables() {
    for old in [17, 19] {
        let p = std::env::temp_dir().join(format!("ddc_sheets_v{old}_{}.ddc", std::process::id()));
        let p = p.to_str().unwrap().to_string();
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{p}{ext}"));
        }
        {
            let mut m = Model::open(&p).unwrap();
            m.add_level("L1", 0.0).unwrap();
        }
        let raw = rusqlite::Connection::open(&p).unwrap();
        for (t, _) in DDL {
            raw.execute_batch(&format!("DROP TABLE {t};")).unwrap();
        }
        raw.execute("UPDATE meta SET value=?1 WHERE key='schema_version'", [old])
            .unwrap();
        drop(raw);
        let mut m = Model::open(&p).unwrap();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        assert!(crate::schema::VERSION >= 21);
        let s = m.add_sheet(&SheetDef::new("A-101", "Plan")).unwrap();
        m.add_viewport(&vp(s, ViewRef::Plan { level: 1, cut: 1.2 }))
            .unwrap();
        m.assert_integrity();
        drop(m);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{p}{ext}"));
        }
        let _ = std::fs::remove_file(format!("{p}.bak-v{old}"));
    }
}

/// A sheet with fields, a schedule, a legend, a revision and a viewport with a crop.
fn rich_sheet(m: &mut Model) -> (i64, i64) {
    let l1 = m.add_level("L1", 0.0).unwrap();
    let mut d = SheetDef::new("A-101", "Plan");
    d.paper = "A1".into();
    d.landscape = false;
    d.fields.insert("developer".into(), "Smith".into());
    let s = m.add_sheet(&d).unwrap();
    let mut v = vp(s, ViewRef::Plan { level: l1, cut: 1.5 });
    v.crop = Some([0.0, 1.0, 8.0, 9.0]);
    v.scale = 50.0;
    v.center = (120.0, 90.0);
    let vid = m.add_viewport(&v).unwrap();
    m.add_schedule_place(&SchedulePlace {
        sheet: s,
        schedule: "doors".into(),
        pos: (10.0, 20.0),
    })
    .unwrap();
    let lg = m.add_legend("Legend", 100.0).unwrap();
    m.place_legend(s, lg, (30.0, 40.0)).unwrap();
    let rev = Revision {
        mark: "1".into(),
        ..Default::default()
    };
    m.set_revisions(s, &[rev]).unwrap();
    (s, vid)
}

#[test]
fn duplicate_copies_fields_and_schedules() {
    let mut m = Model::open(":memory:").unwrap();
    let (s, _) = rich_sheet(&mut m);
    let c = m.duplicate_sheet(s, false, "copy").unwrap();
    let (a, b) = (m.sheet(s).unwrap(), m.sheet(c).unwrap());
    assert_eq!((&a.paper, a.landscape, &a.fields), (&b.paper, b.landscape, &b.fields));
    assert_eq!(b.name, "Plan copy");
    let p = m.schedule_places(c).unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!((p[0].1.schedule.as_str(), p[0].1.pos), ("doors", (10.0, 20.0)));
    let n: i64 = m
        .sql()
        .query_row(
            "SELECT count(*) FROM anno_legend_place WHERE sheet=?1 AND x=30 AND y=40",
            [c],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(count(&m, "anno_legend_place"), 2);
    m.assert_integrity();
}

#[test]
fn duplicate_with_views_copies_viewports() {
    let mut m = Model::open(":memory:").unwrap();
    let (s, _) = rich_sheet(&mut m);
    let c = m.duplicate_sheet(s, true, "copy").unwrap();
    let (a, b) = (m.viewports(s).unwrap(), m.viewports(c).unwrap());
    assert_eq!(b.len(), 1);
    assert_ne!(a[0].0, b[0].0);
    let mut want = a[0].1.clone();
    want.sheet = c;
    assert_eq!(b[0].1, want);
    assert_eq!(b[0].1.crop, Some([0.0, 1.0, 8.0, 9.0]));
    assert_eq!(m.viewports(s).unwrap(), a);
    m.assert_integrity();
}

#[test]
fn duplicate_title_only_has_no_viewports() {
    let mut m = Model::open(":memory:").unwrap();
    let (s, _) = rich_sheet(&mut m);
    let c = m.duplicate_sheet(s, false, "copy").unwrap();
    assert!(m.viewports(c).unwrap().is_empty());
    assert_eq!(m.viewports(s).unwrap().len(), 1);
}

#[test]
fn duplicate_number_is_next_free() {
    let mut m = Model::open(":memory:").unwrap();
    let (s, _) = rich_sheet(&mut m);
    m.add_sheet(&SheetDef::new("A-102", "x")).unwrap();
    let c = m.duplicate_sheet(s, false, "copy").unwrap();
    assert_eq!(m.sheet(c).unwrap().number, "A-103");
    let d = m.duplicate_sheet(s, false, "copy").unwrap();
    assert_eq!(m.sheet(d).unwrap().number, "A-104");
    assert_eq!(m.sheet(d).unwrap().name, "Plan copy 2");
    assert!(m.duplicate_sheet(9999, false, "copy").is_err());
}

#[test]
fn duplicate_one_undo_restores_all() {
    let mut m = Model::open(":memory:").unwrap();
    let (s, _) = rich_sheet(&mut m);
    let before = (
        count(&m, "sheet"),
        count(&m, "viewport"),
        count(&m, "sheet_schedule"),
        count(&m, "anno_legend_place"),
        count(&m, "sheet_rev"),
    );
    let c = m.duplicate_sheet(s, true, "copy").unwrap();
    assert_eq!(count(&m, "viewport"), 2);
    m.undo().unwrap();
    assert!(m.sheet(c).is_err());
    let after = (
        count(&m, "sheet"),
        count(&m, "viewport"),
        count(&m, "sheet_schedule"),
        count(&m, "anno_legend_place"),
        count(&m, "sheet_rev"),
    );
    assert_eq!(before, after);
    m.assert_integrity();
}

#[test]
fn revisions_not_copied() {
    let mut m = Model::open(":memory:").unwrap();
    let (s, _) = rich_sheet(&mut m);
    let c = m.duplicate_sheet(s, true, "copy").unwrap();
    assert!(m.revisions(c).unwrap().is_empty());
    assert_eq!(m.revisions(s).unwrap().len(), 1);
}
