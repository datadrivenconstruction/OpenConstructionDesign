//! A Rooms row without id never makes a second room in a region that already holds one.

use calamine::Data;
use ddc_model::{LayerSpec, Model, RoomSpec, WallSpec};

use super::tests::{edit, set, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;

/// Level L1 with two closed boxes: A (0..5 x 0..4) holds room 101 "Hall", B (10..15 x 0..4) is empty.
fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m.add_wall_type("ext", &[LayerSpec { material: "brick".into(), thickness: 0.2 }]).unwrap();
    for x0 in [0.0, 10.0] {
        let p = [(x0, 0.0), (x0 + 5.0, 0.0), (x0 + 5.0, 4.0), (x0, 4.0)];
        for k in 0..4 {
            m.add_wall(&WallSpec {
                a: p[k],
                b: p[(k + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        }
    }
    m.add_room(&RoomSpec { level: lv, name: "Hall".into(), number: "101".into(), point: (2.5, 2.0), height: 3.0 })
        .unwrap();
    m
}

fn room_count(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM room", [], |r| r.get(0)).unwrap()
}

/// Book with one new Rooms row per `(number, name, x, y)`, each a copy of the Hall row without id.
fn book(m: &Model, tag: &str, rows: &[(&str, &str, f64, f64)]) -> std::path::PathBuf {
    book_with(m, tag, rows, |_| {})
}

/// Like `book`, and `hall` edits the existing Hall row (row 1 of the Rooms grid) afterwards.
fn book_with(
    m: &Model,
    tag: &str,
    rows: &[(&str, &str, f64, f64)],
    hall: impl Fn(&mut Grid) + 'static,
) -> std::path::PathBuf {
    let src = tmp(&format!("{tag}_src"));
    export_xlsx(m, &src).unwrap();
    let dst = tmp(&format!("{tag}_dst"));
    let rows = rows.to_vec();
    edit(&src, &dst, move |s, g: &mut Grid| {
        if s != "Rooms" {
            return;
        }
        let base = g[1].clone();
        for (number, name, x, y) in &rows {
            g.push(base.clone());
            let i = g.len() - 1;
            set(g, i, "id", Data::Empty);
            set(g, i, "number", Data::String((*number).into()));
            set(g, i, "name", Data::String((*name).into()));
            set(g, i, "point x [m]", Data::Float(*x));
            set(g, i, "point y [m]", Data::Float(*y));
        }
        hall(g);
    });
    dst
}

fn opts() -> ImportOptions {
    ImportOptions { apply: true, delete_missing: false, new_params: false }
}

#[test]
fn new_room_row_inside_existing_room_rejected() {
    let mut m = model();
    let p = book(&m, "rd_inside", &[("102", "Copy", 2.5, 2.0)]);
    let rep = import_xlsx_keyed(&mut m, &p, opts(), &KeyDecisions::default()).unwrap();
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
    assert_eq!(rep.rejected[0].sheet, "Rooms");
    assert_eq!(rep.rejected[0].reason, "point is inside room 101 (Hall)");
    assert_eq!(rep.counts().added, 0);
    assert_eq!(room_count(&m), 1, "nothing written for the refused row");
}

#[test]
fn two_new_rows_same_region_second_rejected() {
    let mut m = model();
    let p = book(&m, "rd_two", &[("201", "First", 12.0, 2.0), ("202", "Second", 13.0, 1.0)]);
    let rep = import_xlsx_keyed(&mut m, &p, opts(), &KeyDecisions::default()).unwrap();
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
    assert_eq!(rep.rejected[0].reason, "point is inside room 201 (First)");
    assert_eq!(rep.counts().added, 1, "the first row still applies");
    assert_eq!(room_count(&m), 2);
}

#[test]
fn new_room_row_in_empty_region_applies() {
    let mut m = model();
    let p = book(&m, "rd_empty", &[("201", "Annex", 12.0, 2.0)]);
    let rep = import_xlsx_keyed(&mut m, &p, opts(), &KeyDecisions::default()).unwrap();
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert_eq!(rep.counts().added, 1);
    assert_eq!(room_count(&m), 2);
}

#[test]
fn point_outside_any_region_unchanged_behaviour() {
    let mut m = model();
    let p = book(&m, "rd_out", &[("301", "Yard", 30.0, 30.0), ("302", "Yard 2", 31.0, 30.0)]);
    let rep = import_xlsx_keyed(&mut m, &p, opts(), &KeyDecisions::default()).unwrap();
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert_eq!(rep.counts().added, 2);
    assert_eq!(room_count(&m), 3);
}

#[test]
fn new_row_in_region_of_room_deleted_in_same_book_applies() {
    let mut m = model();
    let p = book_with(&m, "rd_del", &[("102", "Copy", 2.5, 2.0)], |g| {
        set(g, 1, "delete", Data::String("delete".into()))
    });
    let rep = import_xlsx_keyed(&mut m, &p, opts(), &KeyDecisions::default()).unwrap();
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert_eq!(rep.counts().added, 1);
    assert_eq!(room_count(&m), 1, "Hall deleted, copy created");
}

#[test]
fn new_row_in_region_of_room_moved_away_applies() {
    let mut m = model();
    let p = book_with(&m, "rd_move", &[("102", "Copy", 2.5, 2.0)], |g| {
        set(g, 1, "point x [m]", Data::Float(12.5))
    });
    let rep = import_xlsx_keyed(&mut m, &p, opts(), &KeyDecisions::default()).unwrap();
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert_eq!(rep.counts().added, 1);
    assert_eq!(room_count(&m), 2);
}

/// Exports `m`, lets `f` change the Walls and Rooms grids, returns the edited book.
fn book_edit(m: &Model, tag: &str, f: impl Fn(&str, &mut Grid) + 'static) -> std::path::PathBuf {
    let src = tmp(&format!("{tag}_src"));
    export_xlsx(m, &src).unwrap();
    let dst = tmp(&format!("{tag}_dst"));
    edit(&src, &dst, move |s, g: &mut Grid| f(s, g));
    dst
}

fn push_room(g: &mut Grid, number: &str, name: &str, x: f64, y: f64) {
    let base = g[1].clone();
    g.push(base);
    let i = g.len() - 1;
    set(g, i, "id", Data::Empty);
    set(g, i, "number", Data::String(number.into()));
    set(g, i, "name", Data::String(name.into()));
    set(g, i, "point x [m]", Data::Float(x));
    set(g, i, "point y [m]", Data::Float(y));
}

#[test]
fn moved_room_into_occupied_region_refused() {
    let mut m = model();
    // Hall leaves box A for box B; a new row in A is free now, a new row in B is not
    let p = book_edit(&m, "rd_moved", |s, g| {
        if s != "Rooms" {
            return;
        }
        set(g, 1, "point x [m]", Data::Float(12.0));
        set(g, 1, "point y [m]", Data::Float(2.0));
        push_room(g, "102", "Left", 2.5, 2.0);
        push_room(g, "103", "Right", 13.0, 1.0);
    });
    let rep = import_xlsx_keyed(&mut m, &p, opts(), &KeyDecisions::default()).unwrap();
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
    assert_eq!(rep.rejected[0].reason, "point is inside room 101 (Hall)");
    assert_eq!(rep.counts().added, 1);
    assert_eq!(room_count(&m), 2);
}

#[test]
fn new_wall_splitting_region_allows_second_room() {
    let mut m = model();
    // a wall across box A at x = 2.5 makes two regions; Hall moves to the left one
    let p = book_edit(&m, "rd_split", |s, g| match s {
        "Walls" => {
            let base = g[1].clone();
            g.push(base);
            let i = g.len() - 1;
            set(g, i, "id", Data::Empty);
            set(g, i, "ax [m]", Data::Float(2.5));
            set(g, i, "ay [m]", Data::Float(0.0));
            set(g, i, "bx [m]", Data::Float(2.5));
            set(g, i, "by [m]", Data::Float(4.0));
        }
        "Rooms" => {
            set(g, 1, "point x [m]", Data::Float(1.0));
            push_room(g, "102", "Right half", 4.0, 2.0);
        }
        _ => {}
    });
    let rep = import_xlsx_keyed(&mut m, &p, opts(), &KeyDecisions::default()).unwrap();
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert_eq!(rep.counts().added, 2, "the wall and the room");
    assert_eq!(room_count(&m), 2);
}

#[test]
fn unchanged_book_still_refuses_duplicate() {
    let mut m = model();
    let p = book(&m, "rd_same", &[("102", "Copy", 1.0, 1.0)]);
    let rep = import_xlsx_keyed(&mut m, &p, opts(), &KeyDecisions::default()).unwrap();
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
    assert_eq!(rep.rejected[0].reason, "point is inside room 101 (Hall)");
    // same with an unrelated staged wall edit, which sends the check through the staged copy
    let mut m = model();
    let p = book_edit(&m, "rd_same_wall", |s, g| match s {
        "Walls" => set(g, 1, "height [m]", Data::Float(3.5)),
        "Rooms" => push_room(g, "102", "Copy", 1.0, 1.0),
        _ => {}
    });
    let rep = import_xlsx_keyed(&mut m, &p, opts(), &KeyDecisions::default()).unwrap();
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
    assert_eq!(rep.rejected[0].reason, "point is inside room 101 (Hall)");
    assert_eq!(room_count(&m), 1);
}

#[test]
fn new_row_in_region_of_room_dropped_from_book_applies() {
    let mut m = model();
    // the Hall row is gone and missing rows are deleted, so box A is free again
    let p = book_with(&m, "rd_drop", &[("102", "Copy", 2.5, 2.0)], |g| {
        g.remove(1);
    });
    let o = ImportOptions { delete_missing: true, ..opts() };
    let rep = import_xlsx_keyed(&mut m, &p, o, &KeyDecisions::default()).unwrap();
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert_eq!(rep.counts().added, 1);
    assert_eq!(room_count(&m), 1, "Hall deleted, copy created");
}
