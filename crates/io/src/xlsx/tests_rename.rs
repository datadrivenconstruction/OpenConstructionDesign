//! Name swaps and rename cycles of levels and wall types apply in one step; a real name clash
//! and an unreadable id are reported per row and never delete anything.

use calamine::Data;
use ddc_model::{LayerSpec, Model, WallSpec};

use super::tests::{edit, set, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;

/// Levels L1..L3, wall types ext/int/core, four walls of `ext` on L1.
fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    m.add_level("L2", 3.0).unwrap();
    m.add_level("L3", 6.0).unwrap();
    let layer = |t| [LayerSpec { material: "brick".into(), thickness: t }];
    let t = m.add_wall_type("ext", &layer(0.3)).unwrap();
    m.add_wall_type("int", &layer(0.1)).unwrap();
    m.add_wall_type("core", &layer(0.2)).unwrap();
    let p = [(0., 0.), (5., 0.), (5., 4.), (0., 4.)];
    for i in 0..4 {
        let w = WallSpec {
            a: p[i],
            b: p[(i + 1) % 4],
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        m.add_wall(&w).unwrap();
    }
    m
}

fn id_of(m: &Model, table: &str, name: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT id FROM {table} WHERE name=?1"), [name], |r| r.get(0))
        .unwrap()
}

fn name_of(m: &Model, table: &str, id: i64) -> String {
    m.sql()
        .query_row(&format!("SELECT name FROM {table} WHERE id=?1"), [id], |r| r.get(0))
        .unwrap()
}

fn walls(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap()
}

/// Grid row whose id cell is `id`.
fn row_of(g: &Grid, id: i64) -> usize {
    let c = g[0].iter().position(|d| d.to_string() == "id").unwrap();
    (1..g.len())
        .find(|r| match g[*r].get(c) {
            Some(Data::Float(x)) => *x == id as f64,
            Some(Data::Int(x)) => *x == id,
            _ => false,
        })
        .unwrap()
}

/// Exports `m`, renames `(id, new name)` on `sheet`, imports back.
fn rename(m: &mut Model, tag: &str, sheet: &str, to: &[(i64, &str)]) -> Report {
    let p = tmp(&format!("ren_{tag}_src"));
    export_xlsx(m, &p).unwrap();
    let q = tmp(&format!("ren_{tag}_dst"));
    edit(&p, &q, |s, g| {
        if s == sheet {
            for (id, n) in to {
                let r = row_of(g, *id);
                set(g, r, "name", Data::String((*n).into()));
            }
        }
    });
    import_xlsx(m, &q).unwrap()
}

#[test]
fn level_name_swap_applies() {
    let mut m = model();
    let (a, b) = (id_of(&m, "level", "L1"), id_of(&m, "level", "L2"));
    let rep = rename(&mut m, "lswap", LEVELS, &[(a, "L2"), (b, "L1")]);
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!((name_of(&m, "level", a), name_of(&m, "level", b)), ("L2".into(), "L1".into()));
    let on_a: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM wall WHERE level=?1", [a], |r| r.get(0))
        .unwrap();
    assert_eq!(on_a, 4, "walls keep their level id");
}

#[test]
fn wall_type_name_swap_applies() {
    let mut m = model();
    let (a, b) = (id_of(&m, "wall_type", "ext"), id_of(&m, "wall_type", "int"));
    let rep = rename(&mut m, "tswap", TYPES, &[(a, "int"), (b, "ext")]);
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(name_of(&m, "wall_type", a), "int");
    assert_eq!(name_of(&m, "wall_type", b), "ext");
}

#[test]
fn swap_undo_restores_names() {
    let mut m = model();
    let (a, b) = (id_of(&m, "wall_type", "ext"), id_of(&m, "wall_type", "int"));
    let rep = rename(&mut m, "uswap", TYPES, &[(a, "int"), (b, "ext")]);
    assert!(rep.applied, "{rep:?}");
    m.undo().unwrap();
    assert_eq!(name_of(&m, "wall_type", a), "ext");
    assert_eq!(name_of(&m, "wall_type", b), "int");
    let parked: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM wall_type WHERE name LIKE char(1) || '%'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(parked, 0);
}

#[test]
fn level_rename_cycle_applies() {
    let mut m = model();
    let ids = ["L1", "L2", "L3"].map(|n| id_of(&m, "level", n));
    let rep = rename(&mut m, "cycle", LEVELS, &[(ids[0], "L2"), (ids[1], "L3"), (ids[2], "L1")]);
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    let got = ids.map(|i| name_of(&m, "level", i));
    assert_eq!(got, ["L2", "L3", "L1"].map(String::from));
}

#[test]
fn real_duplicate_name_rejected_readable() {
    let mut m = model();
    let (a, b, c) = (id_of(&m, "level", "L1"), id_of(&m, "level", "L2"), id_of(&m, "level", "L3"));
    // L1 -> L2 while L2 keeps its name; L3 is renamed fine
    let rep = rename(&mut m, "dup", LEVELS, &[(a, "L2"), (c, "L9")]);
    assert!(rep.applied && rep.blocking() == 0, "{rep:?}");
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
    let r = &rep.rejected[0];
    assert_eq!((r.sheet.as_str(), r.column.as_str()), (LEVELS, "name"));
    assert!(r.row > 1 && r.reason == "duplicate level name 'L2'", "{r:?}");
    assert!(!rep.rejected.iter().any(|r| r.reason.contains("UNIQUE")));
    assert_eq!(name_of(&m, "level", a), "L1");
    assert_eq!(name_of(&m, "level", b), "L2");
    assert_eq!(name_of(&m, "level", c), "L9");
}

/// Wall rows: `bad` gets an unreadable id cell, `gone` is removed from the sheet.
fn bad_id_book(m: &Model, tag: &str, bad: i64, gone: i64, v: Data) -> std::path::PathBuf {
    let p = tmp(&format!("badid_{tag}_src"));
    export_xlsx(m, &p).unwrap();
    let q = tmp(&format!("badid_{tag}_dst"));
    edit(&p, &q, |s, g| {
        if s == WALLS {
            let r = row_of(g, bad);
            set(g, r, "id", v.clone());
            let r = row_of(g, gone);
            g.remove(r);
        }
    });
    q
}

#[test]
fn bad_id_row_not_deleted_with_delete_missing() {
    for (tag, v) in [("text", Data::String("7x".into())), ("frac", Data::Float(2.5))] {
        let mut m = model();
        let q = bad_id_book(&m, tag, 2, 3, v);
        let o = ImportOptions { apply: true, delete_missing: true, new_params: false };
        let rep = import_xlsx_with(&mut m, &q, o).unwrap();
        assert_eq!(walls(&m), 4, "{tag}: nothing deleted: {rep:?}");
        assert_eq!(rep.counts().deleted, 0, "{tag}");
        let why = rep
            .rejected
            .iter()
            .find(|r| r.sheet == WALLS && r.reason == super::import::NO_DELETES)
            .unwrap_or_else(|| panic!("{tag}: {:?}", rep.rejected));
        assert!(why.row > 1 && why.column == "id");
        assert!(rep.rejected.iter().any(|r| r.row == why.row && r.reason != super::import::NO_DELETES));
    }
}

#[test]
fn bad_id_without_delete_missing_reported() {
    let mut m = model();
    let q = bad_id_book(&m, "nodel", 2, 3, Data::Float(2.5));
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.blocking() == 0, "{rep:?}");
    assert_eq!(walls(&m), 4);
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
    assert_eq!(rep.rejected[0].reason, "not an integer: 2.5");
}
