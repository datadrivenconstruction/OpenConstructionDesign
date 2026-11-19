//! Formulas the user typed into the book survive a re-export (sheet_view.rs): in the app's own
//! columns while their result is still the model value, otherwise listed as a loss; a formula
//! result is never read as an edit of the element.

use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, DataType, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, RoomSpec, WallSpec};
use rust_xlsxwriter::{Formula, Workbook, Worksheet};

use super::msg::{self, fill_template, match_template};
use super::sheet_view::read_book;
use super::tests::tmp;
use super::*;

type Grid = Vec<Vec<Data>>;

struct Fx {
    m: Model,
    walls: [i64; 3],
    room: i64,
}

/// Walls w1, w2 on L2 and w3 on L1 (so a sort by level moves rows), height 3; one room "Hall".
fn fixture() -> Fx {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let t = m.add_wall_type("ext", &[LayerSpec { material: "brick".into(), thickness: 0.25 }]).unwrap();
    let mut walls = [0; 3];
    for (i, (x, lv)) in [(0.0, l2), (3.0, l2), (6.0, l1)].into_iter().enumerate() {
        let w = WallSpec {
            a: (x, 0.0),
            b: (x + 2.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        walls[i] = m.add_wall(&w).unwrap();
    }
    let room = m
        .add_room(&RoomSpec { level: l1, name: "Hall".into(), number: "101".into(), point: (20.0, 20.0), height: 3.0 })
        .unwrap();
    Fx { m, walls, room }
}

fn put(ws: &mut Worksheet, i: u32, j: u16, d: &Data) {
    match d {
        Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
        Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
        Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
        Data::Bool(b) => ws.write_boolean(i, j, *b).map(|_| ()),
        _ => Ok(()),
    }
    .unwrap();
}

fn grid(p: &Path, sheet: &str) -> Grid {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.worksheet_range(sheet).unwrap().rows().map(|r| r.to_vec()).collect()
}

fn col_of(g: &Grid, head: &str) -> u16 {
    g[0].iter().position(|d| units::norm(&d.to_string()) == head).unwrap() as u16
}

fn id_of(row: &[Data]) -> Option<i64> {
    match row.first() {
        Some(Data::Float(x)) => Some(*x as i64),
        Some(Data::Int(x)) => Some(*x),
        _ => None,
    }
}

/// 0-based sheet row of element `id` in `g`.
fn row_of(g: &Grid, id: i64) -> u32 {
    g.iter().position(|r| id_of(r) == Some(id)).unwrap() as u32
}

/// Copy of `src` as a user leaves it: values of every sheet, Walls sorted by level when `sort`,
/// then `dress` writes its formulas over the copy of each sheet.
fn edited(src: &Path, tag: &str, sort: bool, dress: impl Fn(&str, &Grid, &mut Worksheet)) -> PathBuf {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for n in wb.sheet_names() {
        let mut g: Grid = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
        if sort && n == WALLS {
            let lv = col_of(&g, "level") as usize;
            g[1..].sort_by_key(|r| r[lv].to_string());
        }
        let ws = out.add_worksheet();
        ws.set_name(&n).unwrap();
        for (i, row) in g.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                put(ws, i as u32, j as u16, d);
            }
        }
        dress(&n, &g, ws);
    }
    let q = tmp(tag);
    out.save(&q).unwrap();
    q
}

fn formula(ws: &mut Worksheet, r: u32, c: u16, f: &str, res: &str) {
    ws.write_formula(r, c, Formula::new(f).set_result(res)).unwrap();
}

/// Formula text of a cell without the leading "=", None when it holds none.
fn formula_at(p: &Path, sheet: &str, r: u32, c: u16) -> Option<String> {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    let f = wb.worksheet_formula(sheet).ok()?;
    let t = f.get_value((r, c as u32))?.trim().trim_start_matches('=').to_string();
    (!t.is_empty()).then_some(t)
}

fn reexport(m: &Model, old: &Path, tag: &str) -> (PathBuf, Vec<String>) {
    let new = tmp(tag);
    export_xlsx_families_from(m, old, &new, Units::Metric, &[]).unwrap();
    let lost = reexport_losses(old, &new, &[], 0).unwrap();
    (new, lost)
}

fn first(m: &Model, tag: &str) -> PathBuf {
    let p = tmp(tag);
    export_xlsx(m, &p).unwrap();
    p
}

fn height(m: &Model, id: i64) -> f64 {
    m.sql().query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0)).unwrap()
}

fn formula_items(lost: &[String]) -> Vec<&String> {
    lost.iter().filter(|l| match_template(msg::FORMULAS_DROPPED.1, l).is_some()).collect()
}

/// Walls book of `src` with Height of w1 = `f` (stored result `res`).
fn height_formula(src: &Path, tag: &str, w1: i64, f: &'static str, res: &'static str) -> PathBuf {
    edited(src, tag, false, move |n, g, ws| {
        if n == WALLS {
            formula(ws, row_of(g, w1), col_of(g, "height"), f, res);
        }
    })
}

#[test]
fn height_formula_kept_while_model_equals_its_result() {
    let mut fx = fixture();
    let w1 = fx.walls[0];
    let p0 = first(&fx.m, "xs19_keep0");
    let p1 = height_formula(&p0, "xs19_keep1", w1, "=1+2", "3");
    let (p2, lost) = reexport(&fx.m, &p1, "xs19_keep2");
    assert!(formula_items(&lost).is_empty(), "{lost:?}");
    let g = grid(&p2, WALLS);
    let (r, c) = (row_of(&g, w1), col_of(&g, "height"));
    assert_eq!(formula_at(&p2, WALLS, r, c).as_deref(), Some("1+2"));
    assert_eq!(g[r as usize][c as usize].as_f64(), Some(3.0), "the stored result is the model value");
    // the kept formula reads back as no change, and a second re-export keeps it again
    let rep = import_xlsx(&mut fx.m, &p2).unwrap();
    assert!(rep.changes.iter().all(|ch| ch.id != Some(w1)), "{:?}", rep.changes);
    assert!(rep.ignored.iter().all(|i| i.reason != msg::FORMULA_LEFT.1), "{:?}", rep.ignored);
    assert_eq!(height(&fx.m, w1), 3.0);
    let (p3, lost) = reexport(&fx.m, &p2, "xs19_keep3");
    assert!(formula_items(&lost).is_empty(), "{lost:?}");
    let g = grid(&p3, WALLS);
    assert_eq!(formula_at(&p3, WALLS, row_of(&g, w1), col_of(&g, "height")).as_deref(), Some("1+2"));
}

#[test]
fn model_edit_since_replaces_the_formula_and_names_it() {
    let fx = fixture();
    let w1 = fx.walls[0];
    let p0 = first(&fx.m, "xs19_edit0");
    let p1 = height_formula(&p0, "xs19_edit1", w1, "=1+2", "3");
    fx.m.sql().execute("UPDATE wall SET height=4.0 WHERE id=?1", [w1]).unwrap();
    let (p2, lost) = reexport(&fx.m, &p1, "xs19_edit2");
    let g1 = grid(&p1, WALLS);
    let shown = g1[0][col_of(&g1, "height") as usize].to_string();
    let want = fill_template(msg::FORMULAS_DROPPED.1, &["1", shown.trim(), WALLS]);
    assert_eq!(formula_items(&lost), vec![&want], "{lost:?}");
    // the item is one more confirm step of the refusal; once confirmed the new book holds 4
    assert!(refusal(&lost, &[]).is_some_and(|e| e.contains(&want)));
    assert_eq!(refusal(&lost, &lost), None);
    let g = grid(&p2, WALLS);
    let (r, c) = (row_of(&g, w1), col_of(&g, "height"));
    assert_eq!(formula_at(&p2, WALLS, r, c), None);
    assert_eq!(g[r as usize][c as usize].as_f64(), Some(4.0));
}

#[test]
fn formula_result_in_a_model_column_is_reported_not_applied() {
    let mut fx = fixture();
    let w1 = fx.walls[0];
    let p0 = first(&fx.m, "xs19_imp0");
    let p1 = height_formula(&p0, "xs19_imp1", w1, "=2+2", "4");
    let rep = import_xlsx(&mut fx.m, &p1).unwrap();
    assert!(rep.changes.iter().all(|ch| ch.id != Some(w1)), "{:?}", rep.changes);
    let left: Vec<_> = rep.ignored.iter().filter(|i| i.reason == msg::FORMULA_LEFT.1).collect();
    assert_eq!(left.len(), 1, "{:?}", rep.ignored);
    assert_eq!((left[0].id, left[0].sheet.as_str()), (Some(w1), WALLS));
    assert_eq!(height(&fx.m, w1), 3.0, "the model value stays");
    // a plain typed value in the same cell still is an edit
    let p2 = edited(&p0, "xs19_imp2", false, |n, g, ws| {
        if n == WALLS {
            ws.write_number(row_of(g, w1), col_of(g, "height"), 4.0).unwrap();
        }
    });
    let mut m2 = fixture().m;
    let rep = import_xlsx(&mut m2, &p2).unwrap();
    assert!(rep.ignored.iter().all(|i| i.reason != msg::FORMULA_LEFT.1));
    assert_eq!(height(&m2, w1), 4.0);
}

#[test]
fn text_formula_round_trips() {
    let mut fx = fixture();
    let room = fx.room;
    let p0 = first(&fx.m, "xs19_txt0");
    let p1 = edited(&p0, "xs19_txt1", false, move |n, g, ws| {
        if n == ROOMS {
            formula(ws, row_of(g, room), col_of(g, "name"), "=\"Ha\"&\"ll\"", "Hall");
        }
    });
    let (p2, lost) = reexport(&fx.m, &p1, "xs19_txt2");
    assert!(formula_items(&lost).is_empty(), "{lost:?}");
    let g = grid(&p2, ROOMS);
    let (r, c) = (row_of(&g, room), col_of(&g, "name"));
    assert_eq!(formula_at(&p2, ROOMS, r, c).as_deref(), Some("\"Ha\"&\"ll\""));
    assert_eq!(g[r as usize][c as usize], Data::String("Hall".into()));
    let rep = import_xlsx(&mut fx.m, &p2).unwrap();
    assert!(rep.changes.iter().all(|ch| ch.id != Some(room)), "{:?}", rep.changes);
}

#[test]
fn user_column_formula_survives_two_round_trips_and_follows_moved_rows() {
    let fx = fixture();
    let p0 = first(&fx.m, "xs19_user0");
    // sorted by level the rows are w3 (L1), w1, w2 (L2): model order and book order differ
    let p1 = edited(&p0, "xs19_user1", true, |n, g, ws| {
        if n != WALLS {
            return;
        }
        let hc = col_of(g, "height");
        let uc = g[0].len() as u16;
        ws.write_string(0, uc, "double").unwrap();
        let letter = (b'A' + hc as u8) as char;
        for r in 1..g.len() as u32 {
            formula(ws, r, uc, &format!("={letter}{}*2", r + 1), "6");
        }
    });
    let mut old = p1.clone();
    for round in 0..2 {
        let (p, lost) = reexport(&fx.m, &old, &format!("xs19_user{}", round + 2));
        assert!(lost.iter().all(|l| !l.contains("'double'")), "{lost:?}");
        let g = grid(&p, WALLS);
        let (hc, uc) = (col_of(&g, "height"), col_of(&g, "double"));
        let letter = (b'A' + hc as u8) as char;
        for id in fx.walls {
            let r = row_of(&g, id);
            // each formula still points at its own row, wherever the row went
            assert_eq!(formula_at(&p, WALLS, r, uc), Some(format!("{letter}{}*2", r + 1)), "round {round}, wall {id}");
        }
        old = p;
    }
}

#[test]
fn model_formula_on_a_moved_row_points_at_its_new_row() {
    let fx = fixture();
    let w1 = fx.walls[0];
    let p0 = first(&fx.m, "xs19_mv0");
    let p1 = edited(&p0, "xs19_mv1", true, move |n, g, ws| {
        if n == WALLS {
            let (r, c) = (row_of(g, w1), col_of(g, "height"));
            let bo = (b'A' + col_of(g, "base_offset") as u8) as char;
            formula(ws, r, c, &format!("={bo}{}+3", r + 1), "3");
        }
    });
    let (p2, lost) = reexport(&fx.m, &p1, "xs19_mv2");
    assert!(formula_items(&lost).is_empty(), "{lost:?}");
    let g = grid(&p2, WALLS);
    let (r, c) = (row_of(&g, w1), col_of(&g, "height"));
    let bo = (b'A' + col_of(&g, "base_offset") as u8) as char;
    assert_eq!(formula_at(&p2, WALLS, r, c), Some(format!("{bo}{}+3", r + 1)));
}

#[test]
fn book_without_formulas_is_unchanged() {
    let fx = fixture();
    let p0 = first(&fx.m, "xs19_none0");
    assert!(read_book(&p0).values().all(|v| v.formulas.is_empty()));
    let (p1, lost) = reexport(&fx.m, &p0, "xs19_none1");
    assert!(formula_items(&lost).is_empty(), "{lost:?}");
    for sheet in [WALLS, ROOMS] {
        assert_eq!(grid(&p0, sheet), grid(&p1, sheet), "{sheet}");
        let g = grid(&p1, sheet);
        for r in 0..g.len() as u32 {
            for c in 0..g[0].len() as u16 {
                assert_eq!(formula_at(&p1, sheet, r, c), None, "{sheet} {r} {c}");
            }
        }
    }
}

#[test]
fn idless_rows_and_error_constants_are_left_alone() {
    let fx = fixture();
    let w1 = fx.walls[0];
    let p0 = first(&fx.m, "xs19_skip0");
    let p1 = edited(&p0, "xs19_skip1", false, move |n, g, ws| {
        if n != WALLS {
            return;
        }
        let hc = col_of(g, "height");
        // a new row without an id, and an error constant in a model column
        formula(ws, g.len() as u32, hc, "=1+2", "3");
        formula(ws, row_of(g, w1), hc, "=#N/A", "#N/A");
    });
    let v = read_book(&p1).remove(WALLS).unwrap();
    assert!(v.formulas.is_empty(), "{:?}", v.formulas);
    // an error constant in a user column still comes back as that error
    let p2 = edited(&p0, "xs19_skip2", false, |n, g, ws| {
        if n == WALLS {
            let uc = g[0].len() as u16;
            ws.write_string(0, uc, "check").unwrap();
            formula(ws, 1, uc, "=#N/A", "#N/A");
        }
    });
    let (p3, _) = reexport(&fx.m, &p2, "xs19_skip3");
    let g = grid(&p3, WALLS);
    let c = col_of(&g, "check");
    assert_eq!(formula_at(&p3, WALLS, 1, c).as_deref(), Some("#N/A"), "{:?}", g[1]);
}

#[test]
fn formula_texts_have_en_and_ru() {
    let cat = |src: &str| -> std::collections::BTreeMap<String, String> { serde_json::from_str(src).unwrap() };
    let en = cat(include_str!("../../../ui/data/i18n/en/xl_formulas.json"));
    let ru = cat(include_str!("../../../ui/data/i18n/ru/xl_formulas.json"));
    assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
    let cyr = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c));
    for (key, tpl) in msg::FORMULA_REASONS {
        assert_eq!(en.get(key).map(String::as_str), Some(tpl), "{key}: en text is the template");
        let r = &ru[key];
        assert!(cyr(r), "{key}: {r}");
        assert_eq!(tpl.matches("{}").count(), r.matches("{}").count(), "{key}");
    }
    let item = fill_template(msg::FORMULAS_DROPPED.1, &["2", "height [m]", WALLS]);
    assert_eq!(match_template(msg::FORMULAS_DROPPED.1, &item), Some(vec!["2", "height [m]", WALLS]));
    // never read as the user-cell item of a column
    assert!(match_template("{} cells of column '{}' on sheet '{}'", &item).is_none());
}
