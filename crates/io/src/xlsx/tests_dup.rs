// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A row copied in the spreadsheet with the same element id never overwrites the original
//! and never loses an edit: unchanged copies are ignored, one edited row is the edit wherever
//! it sits, two differing rows are refused until the user turns the copies into new elements.

use std::path::PathBuf;

use calamine::Data;
use ddc_model::{LayerSpec, Model, RoomSpec, WallSpec};

use crate::xlsx::msg::{self, fill_template, match_template};
use crate::xlsx::tests::{col, edit, set, tmp};
use crate::xlsx::{
    export_xlsx, import_xlsx_keyed, ImportOptions, KeyDecisions, Rejection, Report, BASELINE, LEVELS, ROOMS, WALLS,
};

type Grid = Vec<Vec<Data>>;

const H: &str = "height [m]";

struct Fx {
    m: Model,
    walls: Vec<i64>,
    room: i64,
    level: i64,
}

/// Level L1, four walls (heights 2.5, 2.6, 2.7, 2.8) and one room "Hall".
fn fx() -> Fx {
    let mut m = Model::open(":memory:").unwrap();
    let level = m.add_level("L1", 0.0).unwrap();
    let t = m.add_wall_type("ext", &[LayerSpec { material: "brick".into(), thickness: 0.2 }]).unwrap();
    let walls = (0..4)
        .map(|i| {
            m.add_wall(&WallSpec {
                a: (0., i as f64 * 3.),
                b: (5., i as f64 * 3.),
                level,
                base_offset: 0.0,
                height: 2.5 + 0.1 * i as f64,
                wall_type: t,
                offset0: None,
            })
            .unwrap()
        })
        .collect();
    let room = m
        .add_room(&RoomSpec { level, name: "Hall".into(), number: "101".into(), point: (20.0, 20.0), height: 3.0 })
        .unwrap();
    Fx { m, walls, room, level }
}

fn book(m: &Model, tag: &str, f: impl Fn(&str, &mut Grid)) -> PathBuf {
    let src = tmp(&format!("xs13_{tag}_src"));
    export_xlsx(m, &src).unwrap();
    let dst = tmp(&format!("xs13_{tag}"));
    edit(&src, &dst, f);
    dst
}

fn row_of(g: &Grid, id: i64) -> usize {
    let c = col(g, "id");
    (1..g.len())
        .find(|&r| matches!(g[r].get(c), Some(Data::Float(x)) if *x == id as f64))
        .unwrap_or_else(|| panic!("no row for id {id}"))
}

/// Copy of the row of `id` at grid index `at` (1 = right below the header); `f` edits it.
fn copy_row(g: &mut Grid, id: i64, at: Option<usize>, f: impl Fn(&mut Grid, usize)) -> usize {
    let copy = g[row_of(g, id)].clone();
    let at = at.unwrap_or(g.len());
    g.insert(at, copy);
    f(g, at);
    at
}

fn run(m: &mut Model, p: &PathBuf, apply: bool, dec: &KeyDecisions) -> Report {
    run_opts(m, p, ImportOptions { apply, delete_missing: false, new_params: false }, dec)
}

fn run_opts(m: &mut Model, p: &PathBuf, o: ImportOptions, dec: &KeyDecisions) -> Report {
    import_xlsx_keyed(m, p, o, dec).unwrap()
}

fn none() -> KeyDecisions {
    KeyDecisions::default()
}

fn q_f(m: &Model, sql: &str, id: i64) -> f64 {
    m.sql().query_row(sql, [id], |r| r.get(0)).unwrap()
}

fn height(m: &Model, id: i64) -> f64 {
    q_f(m, "SELECT height FROM wall WHERE id=?1", id)
}

fn count(m: &Model, table: &str) -> i64 {
    m.sql().query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}

fn room_name(m: &Model, id: i64) -> String {
    m.sql().query_row("SELECT name FROM room WHERE id=?1", [id], |r| r.get(0)).unwrap()
}

fn level_name(m: &Model, id: i64) -> String {
    m.sql().query_row("SELECT name FROM level WHERE id=?1", [id], |r| r.get(0)).unwrap()
}

fn walls_dump(m: &Model) -> Vec<String> {
    let mut st = m.sql().prepare("SELECT id,ax,ay,bx,by,height FROM wall ORDER BY id").unwrap();
    let rows = st
        .query_map([], |r| {
            Ok((0..6).map(|i| format!("{:?}", r.get_ref(i).unwrap())).collect::<Vec<_>>().join(";"))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn dup_refusals<'a>(rep: &'a Report, sheet: &str) -> Vec<&'a Rejection> {
    rep.rejected
        .iter()
        .filter(|r| r.sheet == sheet && match_template(msg::DUP_CONFLICT.1, &r.reason).is_some())
        .collect()
}

fn dup_notes(rep: &Report, sheet: &str) -> usize {
    rep.ignored.iter().filter(|i| i.sheet == sheet && i.reason.starts_with("duplicate id")).count()
}

fn no_refusals(rep: &Report) {
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
}

// --- unchanged copies ---

#[test]
fn walls_unchanged_copy_below_or_above_changes_nothing() {
    for (tag, at) in [("w_same_below", None), ("w_same_above", Some(1))] {
        let mut f = fx();
        let w = f.walls[1];
        let before = walls_dump(&f.m);
        let p = book(&f.m, tag, |s, g| {
            if s == WALLS {
                copy_row(g, w, at, |_, _| {});
            }
        });
        let rep = run(&mut f.m, &p, true, &none());
        assert!(rep.applied, "{tag}: {rep:?}");
        no_refusals(&rep);
        assert!(rep.changes.is_empty(), "{tag}: {:?}", rep.changes);
        assert_eq!(walls_dump(&f.m), before, "{tag}");
        assert_eq!(dup_notes(&rep, WALLS), 1, "{tag}: {:?}", rep.ignored);
        let head = format!("duplicate id {w}: same values as row ");
        assert!(rep.ignored.iter().any(|i| i.reason.starts_with(&head)), "{tag}: {:?}", rep.ignored);
    }
}

#[test]
fn duplicate_id_with_identical_edited_values_applies_once() {
    let mut f = fx();
    let w = f.walls[0];
    let p = book(&f.m, "w_twin_edit", |s, g| {
        if s == WALLS {
            let r = row_of(g, w);
            set(g, r, H, Data::Float(4.0));
            copy_row(g, w, None, |_, _| {});
        }
    });
    let rep = run(&mut f.m, &p, true, &none());
    no_refusals(&rep);
    assert!(rep.applied);
    assert!((height(&f.m, w) - 4.0).abs() < 1e-9);
    assert_eq!(rep.changes.iter().filter(|c| c.id == Some(w)).count(), 1, "{:?}", rep.changes);
    assert_eq!(count(&f.m, "wall"), 4);
}

// --- one edited row is the edit, wherever it sits ---

#[test]
fn walls_edited_copy_below_unchanged_original_is_applied_once() {
    let mut f = fx();
    let w = f.walls[2];
    let p = book(&f.m, "w_edit_below", |s, g| {
        if s == WALLS {
            copy_row(g, w, None, |g, n| set(g, n, H, Data::Float(9.0)));
        }
    });
    let rep = run(&mut f.m, &p, true, &none());
    no_refusals(&rep);
    assert!(rep.applied, "{rep:?}");
    assert!((height(&f.m, w) - 9.0).abs() < 1e-9, "edited copy lost");
    assert_eq!(count(&f.m, "wall"), 4, "a copy with an id never creates an element");
    assert_eq!(rep.changes.iter().filter(|c| c.id == Some(w)).count(), 1, "{:?}", rep.changes);
    assert_eq!(dup_notes(&rep, WALLS), 1);
}

#[test]
fn walls_sorted_edited_copy_above_original_is_applied() {
    let mut f = fx();
    let w = f.walls[3];
    let p = book(&f.m, "w_edit_above", |s, g| {
        if s == WALLS {
            copy_row(g, w, Some(1), |g, n| set(g, n, H, Data::Float(6.5)));
        }
    });
    let rep = run(&mut f.m, &p, true, &none());
    no_refusals(&rep);
    assert!((height(&f.m, w) - 6.5).abs() < 1e-9, "the copy above must win over the untouched original");
    assert_eq!(count(&f.m, "wall"), 4);
}

// --- two differing rows: refused, nothing applied for the id ---

#[test]
fn walls_both_edited_refused_model_untouched_preview_matches_apply() {
    let mut f = fx();
    let (w, other) = (f.walls[1], f.walls[0]);
    let before = walls_dump(&f.m);
    let p = book(&f.m, "w_both", |s, g| {
        if s == WALLS {
            let r = row_of(g, w);
            set(g, r, H, Data::Float(7.0));
            copy_row(g, w, None, |g, n| set(g, n, H, Data::Float(8.0)));
            let o = row_of(g, other);
            set(g, o, H, Data::Float(3.3));
        }
    });
    // sheet rows (1-based) of the original of `w` and of its copy at the end
    let rows = {
        use calamine::Reader;
        let mut wb: calamine::Xlsx<_> = calamine::open_workbook(&p).unwrap();
        let g: Grid = wb.worksheet_range(WALLS).unwrap().rows().map(|r| r.to_vec()).collect();
        (row_of(&g, w) as u32 + 1, g.len() as u32)
    };
    let pre = run(&mut f.m, &p, false, &none());
    assert_eq!(walls_dump(&f.m), before, "a preview never writes");
    let refused = dup_refusals(&pre, WALLS);
    assert_eq!(refused.len(), 2, "{:?}", pre.rejected);
    let want = fill_template(msg::DUP_CONFLICT.1, &[&w.to_string(), &format!("{}, {}", rows.0, rows.1)]);
    assert!(refused.iter().all(|r| r.reason == want), "{refused:?} vs {want}");
    assert_eq!(refused.iter().map(|r| r.row).collect::<Vec<_>>(), vec![rows.0, rows.1]);
    assert!(refused.iter().all(|r| !r.blocks()), "only this id is held back");
    assert_eq!(crate::xlsx::dup_conflict_rows(&want), Some((w, vec![rows.0, rows.1])));
    // Apply does exactly what the preview showed
    let ap = run(&mut f.m, &p, true, &none());
    assert!(ap.applied, "{ap:?}");
    assert_eq!(format!("{:?}", ap.edits), format!("{:?}", pre.edits));
    assert_eq!(format!("{:?}", ap.rejected), format!("{:?}", pre.rejected));
    assert!((height(&f.m, w) - 2.6).abs() < 1e-9, "a refused id keeps its model value");
    assert!((height(&f.m, other) - 3.3).abs() < 1e-9, "the rest of the book applies");
    assert_eq!(count(&f.m, "wall"), 4);
    f.m.undo().unwrap();
    assert_eq!(walls_dump(&f.m), before, "one undo step reverts the import");
}

#[test]
fn refused_id_is_never_deleted_by_delete_missing() {
    let mut f = fx();
    let w = f.walls[2];
    let p = book(&f.m, "w_both_del", |s, g| {
        if s == WALLS {
            let r = row_of(g, w);
            set(g, r, H, Data::Float(7.0));
            copy_row(g, w, None, |g, n| set(g, n, H, Data::Float(8.0)));
        }
    });
    let o = ImportOptions { apply: true, delete_missing: true, new_params: false };
    let rep = run_opts(&mut f.m, &p, o, &none());
    assert_eq!(dup_refusals(&rep, WALLS).len(), 2);
    assert_eq!(count(&f.m, "wall"), 4, "{:?}", rep.changes);
    assert!((height(&f.m, w) - 2.7).abs() < 1e-9);
}

#[test]
fn book_without_baseline_differing_copies_are_refused_not_picked() {
    let mut f = fx();
    let w = f.walls[0];
    let p = book(&f.m, "w_nobase", |s, g| {
        if s == BASELINE {
            g.clear();
        }
        if s == WALLS {
            copy_row(g, w, None, |g, n| set(g, n, H, Data::Float(9.0)));
        }
    });
    let rep = run(&mut f.m, &p, true, &none());
    assert_eq!(dup_refusals(&rep, WALLS).len(), 2, "{:?}", rep.rejected);
    assert!((height(&f.m, w) - 2.5).abs() < 1e-9);
}

// --- the decision: copies become new elements ---

#[test]
fn decision_create_as_new_makes_one_element_original_unchanged() {
    let mut f = fx();
    let w = f.walls[1];
    let before = walls_dump(&f.m);
    let p = book(&f.m, "w_new", |s, g| {
        if s == WALLS {
            copy_row(g, w, None, |g, n| set(g, n, H, Data::Float(9.0)));
        }
    });
    let n_rows = {
        use calamine::Reader;
        let mut wb: calamine::Xlsx<_> = calamine::open_workbook(&p).unwrap();
        wb.worksheet_range(WALLS).unwrap().rows().count() as u32
    };
    let mut dec = none();
    dec.reject.insert((WALLS.to_string(), n_rows));
    let pre = run(&mut f.m, &p, false, &dec);
    let rep = run(&mut f.m, &p, true, &dec);
    assert!(rep.applied, "{rep:?}");
    assert_eq!(format!("{:?}", rep.edits), format!("{:?}", pre.edits));
    no_refusals(&rep);
    assert_eq!(count(&f.m, "wall"), 5, "one new wall");
    assert!((height(&f.m, w) - 2.6).abs() < 1e-9, "original unchanged");
    let new_h: f64 =
        f.m.sql().query_row("SELECT height FROM wall ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).unwrap();
    assert!((new_h - 9.0).abs() < 1e-9, "the new wall has the copy's values");
    f.m.undo().unwrap();
    assert_eq!(walls_dump(&f.m), before);
}

#[test]
fn decision_on_both_edited_rows_creates_two_and_keeps_the_original() {
    let mut f = fx();
    let w = f.walls[3];
    let p = book(&f.m, "w_new_both", |s, g| {
        if s == WALLS {
            let r = row_of(g, w);
            set(g, r, H, Data::Float(7.0));
            copy_row(g, w, None, |g, n| set(g, n, H, Data::Float(8.0)));
        }
    });
    let pre = run(&mut f.m, &p, false, &none());
    let rows: Vec<u32> = dup_refusals(&pre, WALLS).iter().map(|r| r.row).collect();
    assert_eq!(rows.len(), 2);
    let mut dec = none();
    dec.reject.extend(rows.iter().map(|r| (WALLS.to_string(), *r)));
    let o = ImportOptions { apply: true, delete_missing: true, new_params: false };
    let rep = run_opts(&mut f.m, &p, o, &dec);
    no_refusals(&rep);
    assert!(rep.applied, "{rep:?}");
    assert_eq!(count(&f.m, "wall"), 6, "two new walls, the original kept");
    assert!((height(&f.m, w) - 2.8).abs() < 1e-9);
}

// --- rooms and levels sheets ---

#[test]
fn rooms_unchanged_edited_and_conflicting_copies() {
    // unchanged copy above
    let mut f = fx();
    let rm = f.room;
    let p = book(&f.m, "r_same", |s, g| {
        if s == ROOMS {
            copy_row(g, rm, Some(1), |_, _| {});
        }
    });
    let rep = run(&mut f.m, &p, true, &none());
    no_refusals(&rep);
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
    assert_eq!((count(&f.m, "room"), room_name(&f.m, rm)), (1, "Hall".to_string()));
    // edited copy below
    let p = book(&f.m, "r_edit", |s, g| {
        if s == ROOMS {
            copy_row(g, rm, None, |g, n| set(g, n, "name", Data::String("Lobby".into())));
        }
    });
    let rep = run(&mut f.m, &p, true, &none());
    no_refusals(&rep);
    assert_eq!((count(&f.m, "room"), room_name(&f.m, rm)), (1, "Lobby".to_string()));
    // both edited
    let p = book(&f.m, "r_both", |s, g| {
        if s == ROOMS {
            let r = row_of(g, rm);
            set(g, r, "name", Data::String("Office".into()));
            copy_row(g, rm, None, |g, n| set(g, n, "name", Data::String("Store".into())));
        }
    });
    let rep = run(&mut f.m, &p, true, &none());
    assert_eq!(dup_refusals(&rep, ROOMS).len(), 2, "{:?}", rep.rejected);
    assert_eq!((count(&f.m, "room"), room_name(&f.m, rm)), (1, "Lobby".to_string()));
}

#[test]
fn levels_unchanged_edited_conflicting_and_new_copies() {
    let mut f = fx();
    let lv = f.level;
    let p = book(&f.m, "l_same", |s, g| {
        if s == LEVELS {
            copy_row(g, lv, Some(1), |_, _| {});
        }
    });
    let rep = run(&mut f.m, &p, true, &none());
    no_refusals(&rep);
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
    let p = book(&f.m, "l_edit", |s, g| {
        if s == LEVELS {
            copy_row(g, lv, Some(1), |g, n| set(g, n, "name", Data::String("Ground".into())));
        }
    });
    let rep = run(&mut f.m, &p, true, &none());
    no_refusals(&rep);
    assert_eq!((count(&f.m, "level"), level_name(&f.m, lv)), (1, "Ground".to_string()));
    let p = book(&f.m, "l_both", |s, g| {
        if s == LEVELS {
            let r = row_of(g, lv);
            set(g, r, "name", Data::String("A".into()));
            copy_row(g, lv, None, |g, n| {
                set(g, n, "name", Data::String("L2".into()));
                set(g, n, "elevation [m]", Data::Float(3.0));
            });
        }
    });
    let pre = run(&mut f.m, &p, false, &none());
    let refused = dup_refusals(&pre, LEVELS);
    assert_eq!(refused.len(), 2, "{:?}", pre.rejected);
    let copy_row_no = refused.iter().map(|r| r.row).max().unwrap();
    let rep = run(&mut f.m, &p, true, &none());
    assert_eq!((count(&f.m, "level"), level_name(&f.m, lv)), (1, "Ground".to_string()));
    assert_eq!(format!("{:?}", rep.edits), format!("{:?}", pre.edits));
    // the copy becomes level L2; the original row is then the only edit of the id
    let mut dec = none();
    dec.reject.insert((LEVELS.to_string(), copy_row_no));
    let rep = run(&mut f.m, &p, true, &dec);
    no_refusals(&rep);
    assert_eq!((count(&f.m, "level"), level_name(&f.m, lv)), (2, "A".to_string()));
    f.m.undo().unwrap();
    assert_eq!((count(&f.m, "level"), level_name(&f.m, lv)), (1, "Ground".to_string()));
}

#[test]
fn same_id_on_two_sheets_is_not_a_duplicate() {
    let mut f = fx();
    let (w, rm) = (f.walls[0], f.room);
    let p = book(&f.m, "x_sheets", |s, g| {
        if s == WALLS {
            let r = row_of(g, w);
            set(g, r, H, Data::Float(3.5));
        }
        if s == ROOMS {
            let r = row_of(g, rm);
            set(g, r, "name", Data::String("Lobby".into()));
        }
    });
    let rep = run(&mut f.m, &p, true, &none());
    no_refusals(&rep);
    assert_eq!(dup_notes(&rep, WALLS) + dup_notes(&rep, ROOMS), 0, "{:?}", rep.ignored);
    assert!((height(&f.m, w) - 3.5).abs() < 1e-9);
    assert_eq!(room_name(&f.m, rm), "Lobby");
}

#[test]
fn rooms_copy_as_new_never_doubles_the_room_at_its_point() {
    let mut f = fx();
    let rm = f.room;
    let p = book(&f.m, "r_new_same", |s, g| {
        if s == ROOMS {
            let r = row_of(g, rm);
            set(g, r, "name", Data::String("Office".into()));
            copy_row(g, rm, None, |g, n| set(g, n, "name", Data::String("Store".into())));
        }
    });
    // refused, and a refused room is never deleted as missing
    let o = ImportOptions { apply: true, delete_missing: true, new_params: false };
    let rep = run_opts(&mut f.m, &p, o, &none());
    let rows: Vec<u32> = dup_refusals(&rep, ROOMS).iter().map(|r| r.row).collect();
    assert_eq!(rows.len(), 2, "{:?}", rep.rejected);
    assert_eq!((count(&f.m, "room"), room_name(&f.m, rm)), (1, "Hall".to_string()));
    // both rows turned new sit on the room's own point: refused, never a second room there
    let mut dec = none();
    dec.reject.extend(rows.iter().map(|r| (ROOMS.to_string(), *r)));
    let pre = run(&mut f.m, &p, false, &dec);
    let rep = run(&mut f.m, &p, true, &dec);
    assert_eq!(format!("{:?}", rep.edits), format!("{:?}", pre.edits));
    assert_eq!(format!("{:?}", rep.rejected), format!("{:?}", pre.rejected));
    let at_point = rep.rejected.iter().filter(|r| r.sheet == ROOMS && r.reason.starts_with("point is inside room"));
    assert_eq!(at_point.count(), 2, "{:?}", rep.rejected);
    assert_eq!((count(&f.m, "room"), room_name(&f.m, rm)), (1, "Hall".to_string()));
}

#[test]
fn rooms_copy_as_new_at_another_point_creates_one_room() {
    let mut f = fx();
    let rm = f.room;
    let p = book(&f.m, "r_new_moved", |s, g| {
        if s == ROOMS {
            copy_row(g, rm, None, |g, n| {
                set(g, n, "name", Data::String("Store".into()));
                set(g, n, "number", Data::String("102".into()));
                set(g, n, "point x [m]", Data::Float(40.0));
            });
        }
    });
    let n_rows = {
        use calamine::Reader;
        let mut wb: calamine::Xlsx<_> = calamine::open_workbook(&p).unwrap();
        wb.worksheet_range(ROOMS).unwrap().rows().count() as u32
    };
    let mut dec = none();
    dec.reject.insert((ROOMS.to_string(), n_rows));
    let pre = run(&mut f.m, &p, false, &dec);
    let rep = run(&mut f.m, &p, true, &dec);
    no_refusals(&rep);
    assert!(rep.applied, "{rep:?}");
    assert_eq!(format!("{:?}", rep.edits), format!("{:?}", pre.edits));
    assert_eq!((count(&f.m, "room"), room_name(&f.m, rm)), (2, "Hall".to_string()));
    let x = q_f(&f.m, "SELECT px FROM room WHERE id<>?1", rm);
    assert!((x - 40.0).abs() < 1e-9, "the new room has the copy's point");
}

// --- pure pick and locale ---

#[test]
fn dup_pick_without_baseline_compares_the_rows() {
    use super::dup::{dup_pick, Pick};
    use std::collections::{HashMap, HashSet};
    let row = |id: f64, h: f64| vec![Data::Float(id), Data::Float(h)];
    let tab = |rows: Vec<Vec<Data>>| super::Tab {
        name: WALLS,
        r0: 1,
        c0: 0,
        head: vec!["id".into(), H.into()],
        idx: [("id", 0), ("height", 1)].into_iter().collect(),
        headers: HashMap::new(),
        factors: HashMap::new(),
        dims: HashMap::new(),
        rows: rows.into_iter().enumerate().map(|(i, c)| (i as u32 + 2, c)).collect(),
        base: None,
        also_known: HashSet::new(),
        merged_id: None,
        covered: HashSet::new(),
        formulas: HashSet::new(),
    };
    let t = tab(vec![row(5.0, 3.0), row(5.0, 3.0)]);
    assert_eq!(dup_pick(&t, 5, &[0, 1]), Pick::One(0));
    let t = tab(vec![row(5.0, 3.0), row(5.0, 4.0)]);
    assert_eq!(dup_pick(&t, 5, &[0, 1]), Pick::Conflict(vec![0, 1]));
}

#[test]
fn dup_reasons_have_en_and_ru_texts() {
    let cat = |src: &str| -> std::collections::BTreeMap<String, String> { serde_json::from_str(src).unwrap() };
    let en = cat(include_str!("../../../ui/data/i18n/en/xlsx_dup.json"));
    let ru = cat(include_str!("../../../ui/data/i18n/ru/xlsx_dup.json"));
    assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
    let cyr = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c));
    for (key, tpl) in msg::DUP_REASONS {
        assert_eq!(en.get(key).map(String::as_str), Some(tpl), "{key}: en text is the template");
        let r = &ru[key];
        assert!(cyr(r), "{key}: {r}");
        assert_eq!(tpl.matches("{}").count(), r.matches("{}").count(), "{key}");
    }
    // the two refusal texts never read as each other
    let keep = fill_template(msg::DUP_CONFLICT_KEEP.1, &["3", "2, 9"]);
    assert!(match_template(msg::DUP_CONFLICT.1, &keep).is_none());
    assert_eq!(crate::xlsx::dup_conflict_rows(&keep), None);
}
