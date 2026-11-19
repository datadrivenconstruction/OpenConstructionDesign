//! Export scope: level, selection and categories limit the rows written; a re-import never
//! touches elements outside the scope.

use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, OpeningSpec, WallSpec};
use rust_xlsxwriter::Workbook;

use super::*;

type Grid = Vec<Vec<Data>>;

/// Level L1 has walls 1..=3 (an opening on wall 1), level L2 has wall 4.
fn model() -> (Model, i64, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let layer = LayerSpec {
        material: "brick".into(),
        thickness: 0.2,
    };
    let t = m.add_wall_type("ext", &[layer]).unwrap();
    for (lv, a, b) in [
        (l1, (0., 0.), (5., 0.)),
        (l1, (5., 0.), (5., 4.)),
        (l1, (5., 4.), (0., 4.)),
        (l2, (0., 0.), (5., 0.)),
    ] {
        let w = WallSpec {
            a,
            b,
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        m.add_wall(&w).unwrap();
    }
    let o = OpeningSpec {
        along: 1.0,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    };
    m.add_opening(1, &o).unwrap();
    (m, l1, l2)
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("ddc_xlsx_tests");
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{}_scope_{name}.xlsx", std::process::id()))
}

fn export_scoped(m: &Model, name: &str, sc: Scope) -> PathBuf {
    let p = tmp(name);
    export_xlsx_styled(m, &p, &p, Units::Metric, &[], &Style::default().with_scope(sc)).unwrap();
    p
}

fn grid(p: &Path, sheet: &str) -> Grid {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.worksheet_range(sheet)
        .unwrap()
        .rows()
        .map(|r| r.to_vec())
        .collect()
}

/// Ids in the first column of a sheet, header skipped.
fn ids_in(p: &Path, sheet: &str) -> Vec<i64> {
    grid(p, sheet)
        .iter()
        .skip(1)
        .filter_map(|r| match r.first() {
            Some(Data::Float(x)) => Some(*x as i64),
            _ => None,
        })
        .collect()
}

fn rewrite(src: &Path, dst: &Path, f: impl Fn(&str, &mut Grid)) {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for name in wb.sheet_names() {
        let mut g = grid(src, &name);
        f(&name, &mut g);
        // an emptied grid drops the sheet
        if g.is_empty() {
            continue;
        }
        let ws = out.add_worksheet();
        ws.set_name(&name).unwrap();
        for (i, row) in g.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::String(t) => ws.write_string(i, j, t).map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(dst).unwrap();
}

fn walls(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap()
}

fn apply(delete_missing: bool) -> ImportOptions {
    ImportOptions {
        apply: true,
        delete_missing,
        new_params: false,
    }
}

#[test]
fn level_scope_exports_only_level_rows() {
    let (m, l1, l2) = model();
    let sc = Scope {
        label: "L2".into(),
        level: Some(l2),
        ..Scope::default()
    };
    let p = export_scoped(&m, "level", sc);
    assert_eq!(ids_in(&p, WALLS), vec![4]);
    assert!(ids_in(&p, OPENINGS).is_empty(), "the only opening sits on a level 1 wall");
    let sc = Scope {
        label: "L1".into(),
        level: Some(l1),
        ..Scope::default()
    };
    let p = export_scoped(&m, "level1", sc);
    assert_eq!(ids_in(&p, WALLS), vec![1, 2, 3]);
    assert_eq!(ids_in(&p, OPENINGS), vec![1], "openings follow their wall's level");
}

#[test]
fn selection_scope_exports_selected_ids() {
    let (m, _, _) = model();
    let sc = Scope {
        label: "selection".into(),
        selected: Some(Selected {
            walls: vec![1, 3],
            ..Selected::default()
        }),
        ..Scope::default()
    };
    let p = export_scoped(&m, "sel", sc);
    assert_eq!(ids_in(&p, WALLS), vec![1, 3]);
    assert!(ids_in(&p, OPENINGS).is_empty(), "an unselected category exports no rows");
}

#[test]
fn category_scope_exports_only_chosen_categories() {
    let (m, l1, _) = model();
    let sc = Scope {
        label: "L1, walls".into(),
        level: Some(l1),
        cats: Some(vec![Cat::Walls]),
        ..Scope::default()
    };
    let p = export_scoped(&m, "cats", sc);
    assert_eq!(ids_in(&p, WALLS), vec![1, 2, 3]);
    assert!(ids_in(&p, OPENINGS).is_empty());
}

#[test]
fn delete_missing_ignores_out_of_scope() {
    let (mut m, l1, _) = model();
    let sc = Scope {
        label: "L1".into(),
        level: Some(l1),
        ..Scope::default()
    };
    let p = export_scoped(&m, "del_src", sc);
    // an untouched scoped book removes nothing, with the delete option on
    let rep = import_xlsx_with(&mut m, &p, apply(true)).unwrap();
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert_eq!(walls(&m), 4, "wall 4 is outside the scope and stays");
    // a row deleted inside the scope is still removed
    let q = tmp("del_dst");
    rewrite(&p, &q, |sh, g| {
        if sh == WALLS {
            g.remove(2);
        }
    });
    let rep = import_xlsx_with(&mut m, &q, apply(true)).unwrap();
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
    assert_eq!(walls(&m), 3, "only the deleted row's wall is gone");
    let left: Vec<i64> = m
        .sql()
        .prepare("SELECT id FROM wall ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(left, vec![1, 3, 4]);
}

#[test]
fn scope_named_in_info_and_preview() {
    let (mut m, l1, _) = model();
    let sc = Scope {
        label: "L1, walls+doors".into(),
        level: Some(l1),
        ..Scope::default()
    };
    let p = export_scoped(&m, "named", sc);
    let info = grid(&p, INFO);
    assert_eq!(info[2][0].to_string(), "scope");
    assert_eq!(info[2][1].to_string(), "L1, walls+doors");
    let rep = import_xlsx_with(&mut m, &p, ImportOptions::default()).unwrap();
    assert_eq!(rep.scope, "L1, walls+doors");
    let whole = tmp("whole");
    export_xlsx(&m, &whole).unwrap();
    // Info also names the project (a8379583) and the decimal mark (fb92e87a): no fixed count
    let keys: Vec<String> = grid(&whole, INFO).iter().map(|r| r[0].to_string()).collect();
    assert!(!keys.iter().any(|k| k.starts_with("scope")), "a whole-model book has no scope row: {keys:?}");
    assert_eq!(import_xlsx_with(&mut m, &whole, ImportOptions::default()).unwrap().scope, "");
}

#[test]
fn scoped_book_without_baseline_deletes_nothing() {
    let (mut m, l1, _) = model();
    let sc = Scope {
        label: "L1".into(),
        level: Some(l1),
        ..Scope::default()
    };
    let p = export_scoped(&m, "nobase_src", sc);
    let q = tmp("nobase_dst");
    rewrite(&p, &q, |sh, g| {
        if sh == BASELINE {
            g.clear();
        }
    });
    import_xlsx_with(&mut m, &q, apply(true)).unwrap();
    assert_eq!(walls(&m), 4, "wall 4 is outside the scope: never deleted");
}

#[test]
fn scope_spec_roundtrips_info_sheet() {
    let (mut m, l1, _) = model();
    let sc = Scope {
        label: "L1, walls".into(),
        level: Some(l1),
        cats: Some(vec![Cat::Walls, Cat::Rooms]),
        ..Scope::default()
    };
    let p = export_scoped(&m, "spec", sc.clone());
    let info = grid(&p, INFO);
    assert_eq!(info[3][0].to_string(), "scope_spec");
    assert_eq!(info[3][1].to_string(), format!("level={l1};cats=walls,rooms"));
    let rep = import_xlsx_with(&mut m, &p, ImportOptions::default()).unwrap();
    assert_eq!(rep.scope_spec, info[3][1].to_string());
    assert_eq!(Scope::from_spec(&rep.scope_spec, &rep.scope, &rep.base_ids), Some(sc));
    // a selection comes back from the Baseline ids
    let sel = Scope {
        label: "selection".into(),
        selected: Some(Selected { walls: vec![1, 3], ..Selected::default() }),
        ..Scope::default()
    };
    let p = export_scoped(&m, "spec_sel", sel);
    let rep = import_xlsx_with(&mut m, &p, ImportOptions::default()).unwrap();
    assert_eq!(rep.scope_spec, "selection");
    let back = Scope::from_spec(&rep.scope_spec, &rep.scope, &rep.base_ids).unwrap();
    assert_eq!(back.selected.unwrap().walls, vec![1, 3]);
    // an older scoped book (label only) narrows to its Baseline, never to the whole model
    let old = Scope::from_spec("", "L1", &Selected { walls: vec![2], ..Selected::default() }).unwrap();
    assert_eq!(old.selected.unwrap().walls, vec![2]);
    assert!(Scope::from_spec("level=x", "L1", &Selected::default()).unwrap().selected.is_some());
}

#[test]
fn whole_book_has_no_scope_spec() {
    let (mut m, _, _) = model();
    let p = tmp("whole_spec");
    export_xlsx(&m, &p).unwrap();
    // Info also names the project (a8379583) and the decimal mark (fb92e87a): no fixed count
    let keys: Vec<String> = grid(&p, INFO).iter().map(|r| r[0].to_string()).collect();
    assert!(!keys.iter().any(|k| k.starts_with("scope")), "{keys:?}");
    let rep = import_xlsx_with(&mut m, &p, ImportOptions::default()).unwrap();
    assert_eq!(rep.scope_spec, "");
    assert_eq!(rep.base_ids, Selected::default());
    assert_eq!(Scope::from_spec(&rep.scope_spec, &rep.scope, &rep.base_ids), None);
    assert_eq!(Scope::default().to_spec(), "");
}

#[test]
fn selection_scope_reexport_keeps_created_rows() {
    let (mut m, _, _) = model();
    let mut sc = Scope {
        label: "selection".into(),
        selected: Some(Selected { walls: vec![1, 2], ..Selected::default() }),
        ..Scope::default()
    };
    let p = export_scoped(&m, "created_src", sc.clone());
    let q = tmp("created_dst");
    rewrite(&p, &q, |s, g| {
        if s == WALLS {
            let col = |h: &str| g[0].iter().position(|d| d.to_string() == h).unwrap();
            let mut row = vec![Data::Empty; g[0].len()];
            row[col("level")] = Data::String("L1".into());
            row[col("type")] = Data::String("ext".into());
            for (h, v) in [("ax [m]", 0.0), ("ay [m]", 10.0), ("bx [m]", 5.0), ("by [m]", 10.0), ("height [m]", 2.8)] {
                row[col(h)] = Data::Float(v);
            }
            g.push(row);
        }
    });
    let before = Selected::of_model(&m).unwrap();
    let rep = import_xlsx_with(&mut m, &q, apply(false)).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(walls(&m), 5);
    let after = Selected::of_model(&m).unwrap();
    // without the join the created wall (id 5) would drop out of the re-export
    let stale = export_scoped(&m, "created_stale", sc.clone());
    assert_eq!(ids_in(&stale, WALLS), vec![1, 2]);
    sc.selected.as_mut().unwrap().join_new(&before, &after);
    assert_eq!(sc.selected.as_ref().unwrap().walls, vec![1, 2, 5]);
    let fresh = export_scoped(&m, "created_fresh", sc);
    assert_eq!(ids_in(&fresh, WALLS), vec![1, 2, 5]);
}

#[test]
fn book_info_names_project_and_ids() {
    let (m, l1, _) = model();
    let uid: i64 = m
        .sql()
        .query_row("SELECT value FROM meta WHERE key='project_uid'", [], |r| r.get(0))
        .unwrap();
    for sc in [None, Some(Scope { level: Some(l1), label: "L1".into(), ..Scope::default() })] {
        let p = tmp(if sc.is_some() { "info_scoped" } else { "info_whole" });
        let st = match sc {
            Some(s) => Style::default().with_scope(s),
            None => Style::default(),
        };
        export_xlsx_styled(&m, &p, &p, Units::Metric, &[], &st).unwrap();
        let info = book_info(&p).unwrap().expect("an Info sheet");
        assert_eq!(info.project, Some(uid));
        assert!(!info.ids.walls.is_empty());
    }
}

#[test]
fn book_info_of_a_foreign_workbook_is_none() {
    let p = tmp("foreign_info");
    let mut wb = Workbook::new();
    wb.add_worksheet().set_name("Sheet1").unwrap();
    wb.save(&p).unwrap();
    assert_eq!(book_info(&p).unwrap(), None);
}

#[test]
fn rows_outside_counts_old_rows_the_new_scope_drops() {
    let (m, _, l2) = model();
    let old = Selected::of_model(&m).unwrap();
    assert_eq!(rows_outside(&m, None, &old).unwrap(), 0);
    let sc = Scope { level: Some(l2), label: "L2".into(), ..Scope::default() };
    // walls 1..=3 and the opening on wall 1 sit on L1
    assert_eq!(rows_outside(&m, Some(&sc), &old).unwrap(), 4);
    let cats = Scope { cats: Some(vec![Cat::Walls]), label: "walls".into(), ..Scope::default() };
    assert_eq!(rows_outside(&m, Some(&cats), &old).unwrap(), 1, "only the opening");
}
