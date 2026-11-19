// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Edge cases of a hand-edited book: sorted rows, pasted duplicates, formulas, comma text,
//! missing key sheet or id column, odd text, all rows removed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, WallSpec};
use rust_xlsxwriter::{Formula, Workbook};

use super::tests::{col, set, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;

fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layers = [LayerSpec {
        material: "brick".into(),
        thickness: 0.2,
    }];
    let t = m.add_wall_type("ext", &layers).unwrap();
    for i in 0..4 {
        let w = WallSpec {
            a: (0., i as f64 * 3.),
            b: (5., i as f64 * 3.),
            level: lv,
            base_offset: 0.0,
            height: 2.5 + 0.1 * i as f64,
            wall_type: t,
            offset0: None,
        };
        m.add_wall(&w).unwrap();
    }
    m
}

fn heights(m: &Model) -> Vec<f64> {
    let mut st = m.sql().prepare("SELECT height FROM wall ORDER BY id").unwrap();
    let rows = st.query_map([], |r| r.get::<_, f64>(0)).unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn walls_snapshot(m: &Model) -> Vec<String> {
    let mut st = m
        .sql()
        .prepare("SELECT id,level,wtype,ax,ay,bx,by,base_offset,height,bulge FROM wall ORDER BY id")
        .unwrap();
    let rows = st
        .query_map([], |r| {
            let mut s = String::new();
            for i in 0..10 {
                s += &format!("{:?};", r.get_ref(i).unwrap());
            }
            Ok(s)
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn export(m: &Model, name: &str) -> PathBuf {
    let p = tmp(&format!("edge_{name}"));
    export_xlsx(m, &p).unwrap();
    p
}

/// Copies a book through its cell values: sheets in `drop` are left out, `f` edits each grid,
/// `formulas` (sheet, row, col, expression, cached result) replace cells with formula cells.
fn rewrite(
    src: &Path,
    dst: &Path,
    drop: &[&str],
    f: impl Fn(&str, &mut Grid),
    formulas: &[(&str, u32, u16, &str, &str)],
) {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for name in wb.sheet_names() {
        if drop.contains(&name.as_str()) {
            continue;
        }
        let mut grid: Grid = wb
            .worksheet_range(&name)
            .unwrap()
            .rows()
            .map(|r| r.to_vec())
            .collect();
        f(&name, &mut grid);
        let ws = out.add_worksheet();
        ws.set_name(&name).unwrap();
        // an empty result stays empty (the writer would put 0): a book nobody calculated
        ws.set_formula_result_default("");
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                    Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                    Data::Bool(b) => ws.write_boolean(i, j, *b).map(|_| ()),
                    // a date cell must reach the book as a date, not vanish into a blank
                    Data::DateTime(d) => ws
                        .write_number_with_format(
                            i,
                            j,
                            d.as_f64(),
                            &rust_xlsxwriter::Format::new().set_num_format("yyyy-mm-dd"),
                        )
                        .map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
        for (sheet, r, c, expr, res) in formulas {
            if *sheet == name {
                ws.write_formula(*r, *c, Formula::new(*expr).set_result(*res))
                    .unwrap();
            }
        }
    }
    if formulas.iter().any(|f| f.4.is_empty()) {
        let raw = dst.with_extension("raw.xlsx");
        out.save(&raw).unwrap();
        patch_sheets(&raw, dst, drop_empty_results);
    } else {
        out.save(dst).unwrap();
    }
}

/// A formula cell written with an empty result (`t="str"`, `<v></v>`) loses the result, as a
/// writer that never calculates leaves it; a cached "" stays only where a test patches it back.
fn drop_empty_results(s: &mut String) {
    const V: &str = "<v></v></c>";
    let mut from = 0;
    while let Some(k) = s[from..].find(V).map(|k| k + from) {
        let c = s[..k].rfind("<c ").unwrap_or(0);
        if s[c..k].contains("<f") {
            s.replace_range(k..k + V.len(), "</c>");
            let open = s[c..k].find('>').map_or(k, |e| c + e);
            if let Some(t) = s[c..open].find(" t=\"str\"").map(|t| t + c) {
                s.replace_range(t..t + 8, "");
            }
            from = c;
        } else {
            from = k + V.len();
        }
    }
}

fn edit(src: &Path, dst: &Path, f: impl Fn(&str, &mut Grid)) {
    rewrite(src, dst, &[], f, &[]);
}

fn row_of(g: &Grid, id: i64) -> usize {
    let c = col(g, "id");
    (1..g.len())
        .find(|&r| matches!(g[r].get(c), Some(Data::Float(x)) if *x == id as f64))
        .unwrap_or_else(|| panic!("no row for id {id}"))
}

fn id_of(r: &[Data], c: usize) -> i64 {
    match r.get(c) {
        Some(Data::Float(x)) => *x as i64,
        _ => i64::MIN,
    }
}

fn apply(m: &mut Model, p: &Path) -> Report {
    import_xlsx(m, p).unwrap()
}

#[test]
fn sorted_rows_pair_by_id() {
    let mut m = model();
    let before = heights(&m);
    let p = export(&m, "sorted_a");
    let q = tmp("edge_sorted_b");
    edit(&p, &q, |s, g| {
        if s == WALLS {
            let c = col(g, "id");
            // descending by id, as after sorting by another column
            g[1..].sort_by(|a, b| id_of(b, c).cmp(&id_of(a, c)));
            let r = row_of(g, 2);
            set(g, r, "height [m]", Data::Float(3.3));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(rep.applied, "{rep:?}");
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert_eq!(rep.changes[0].id, Some(2));
    let mut want = before;
    want[1] = 3.3;
    assert_eq!(heights(&m), want);
}

#[test]
fn duplicate_id_edited_copy_applies_once_rest_applies() {
    let mut m = model();
    let p = export(&m, "dup_a");
    let q = tmp("edge_dup_b");
    edit(&p, &q, |s, g| {
        if s == WALLS {
            let copy = g[row_of(g, 1)].clone();
            g.push(copy);
            let n = g.len() - 1;
            set(g, n, "height [m]", Data::Float(9.0));
            let r = row_of(g, 3);
            set(g, r, "height [m]", Data::Float(3.1));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(rep.applied, "{rep:?}");
    let h = heights(&m);
    assert_eq!(h.len(), 4, "a pasted copy must not create a wall");
    // the untouched original is ignored, the edited copy below it is the edit
    assert!((h[0] - 9.0).abs() < 1e-9, "edited copy lost: {h:?}");
    assert!((h[2] - 3.1).abs() < 1e-9, "{h:?}");
    let ig: Vec<_> = rep
        .ignored
        .iter()
        .filter(|i| i.sheet == WALLS && i.reason.contains("duplicate id 1"))
        .collect();
    assert_eq!(ig.len(), 1, "{:?}", rep.ignored);
    assert!(ig[0].reason.contains("unchanged copy"), "{:?}", ig[0]);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected);
}

#[test]
fn formula_cell_cached_value() {
    let mut m = model();
    let p = export(&m, "formula_a");
    let mut g: Grid = vec![];
    {
        let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
        g.extend(wb.worksheet_range(WALLS).unwrap().rows().map(|r| r.to_vec()));
    }
    let c = col(&g, "height [m]") as u16;
    let (r1, r2) = (row_of(&g, 1) as u32, row_of(&g, 2) as u32);
    let q = tmp("edge_formula_b");
    // a numeric result and a text result, both with the value the spreadsheet cached
    let f = [
        (WALLS, r1, c, "=2+3", "5"),
        (WALLS, r2, c, "=\"3,5\"", "3,5"),
    ];
    rewrite(&p, &q, &[], |_, _| {}, &f);
    let before = heights(&m);
    let rep = apply(&mut m, &q);
    // a formula result is never an edit of the element: both are listed, the model stays
    assert_eq!(formula_left(&rep), 2, "{rep:?}");
    assert_eq!(heights(&m), before, "{rep:?}");
}

/// Formula cells whose result was listed and not applied.
fn formula_left(rep: &Report) -> usize {
    rep.ignored.iter().filter(|i| i.reason == super::msg::FORMULA_LEFT.1).count()
}

#[test]
fn ru_comma_text_number() {
    let mut m = model();
    // a comma book: `3,5` in a point book is an open decision, not tested here
    let p = tmp("edge_comma_a");
    let st = Style::default().with_number_format(true, "ru-RU");
    export_xlsx_styled(&m, &p, &p, Units::Metric, &[], &st).unwrap();
    let q = tmp("edge_comma_b");
    edit(&p, &q, |s, g| {
        if s == WALLS {
            let r = row_of(g, 1);
            set(g, r, "height [m]", Data::String("3,5".into()));
            let r = row_of(g, 2);
            set(g, r, "height [m]", Data::String(" 2,75 ".into()));
            let r = row_of(g, 3);
            set(g, r, "bulge (tan(angle/4), 0 = straight)", Data::String("0,25".into()));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    let h = heights(&m);
    assert!((h[0] - 3.5).abs() < 1e-9 && (h[1] - 2.75).abs() < 1e-9, "{h:?}");
    let b: f64 = m
        .sql()
        .query_row("SELECT bulge FROM wall WHERE id=3", [], |r| r.get(0))
        .unwrap();
    assert!((b - 0.25).abs() < 1e-9, "{b}");
}

#[test]
fn keys_sheet_deleted_reported() {
    let mut m = model();
    let p = tmp("edge_keys_a");
    let caps: HashMap<String, String> = [("height".to_string(), "Hgt".to_string())].into();
    export_xlsx_styled(&m, &p, &p, Units::Metric, &[], &Style::professional(caps)).unwrap();
    assert!(sheet_names(&p).unwrap().iter().any(|n| n == "Keys"));
    let before = walls_snapshot(&m);
    let q = tmp("edge_keys_b");
    // the captioned column can only be named through the key row
    rewrite(
        &p,
        &q,
        &["Keys"],
        |s, g| {
            if s == WALLS {
                set(g, 1, "Hgt [m]", Data::Float(4.0));
            }
        },
        &[],
    );
    let rep = apply(&mut m, &q);
    assert!(!rep.applied && rep.blocking() > 0, "{rep:?}");
    // Layers read before Walls and block too when Keys is gone: this test is about Walls
    let r = rep
        .rejected
        .iter()
        .find(|r| r.blocks() && r.sheet == WALLS && r.column.to_lowercase().contains("height"))
        .unwrap_or_else(|| panic!("{rep:?}"));
    assert!(r.reason.starts_with("required column missing"), "{r:?}");
    assert_eq!(walls_snapshot(&m), before);
}

#[test]
fn id_column_deleted_blocks_with_reason() {
    let mut m = model();
    let p = export(&m, "idcol_a");
    let before = walls_snapshot(&m);
    let q = tmp("edge_idcol_b");
    edit(&p, &q, |s, g| {
        if s == WALLS {
            let r = row_of(g, 1);
            set(g, r, "height [m]", Data::Float(4.0));
            let c = col(g, "id");
            for row in g.iter_mut() {
                if c < row.len() {
                    row.remove(c);
                }
            }
        }
    });
    for apply_it in [false, true] {
        let o = ImportOptions {
            apply: apply_it,
            delete_missing: false,
            new_params: false,
        };
        let rep = import_xlsx_with(&mut m, &q, o).unwrap();
        assert!(!rep.applied && rep.blocking() > 0, "{rep:?}");
        let r = rep
            .rejected
            .iter()
            .find(|r| r.blocks() && r.sheet == WALLS)
            .unwrap();
        assert!(r.reason.starts_with("required column missing"), "{r:?}");
        assert_eq!(r.column, "id");
        assert_eq!(walls_snapshot(&m), before, "nothing may be applied");
    }
}

fn layer_material(m: &Model) -> String {
    m.sql()
        .query_row("SELECT material FROM wall_type_layer WHERE pos=0", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn unicode_long_text_roundtrip() {
    let odd = "\u{41F}\u{440}\u{438}\u{432}\u{435}\u{442} \u{1F3D7}\u{FE0F} <b>&\"</b> \u{4E2D}\u{6587}";
    let long = "\u{416}x".repeat(16_000);
    for (tag, name) in [("uni", odd.to_string()), ("long", long)] {
        let mut m = model();
        let p = export(&m, &format!("text_a_{tag}"));
        let q = tmp(&format!("edge_text_b_{tag}"));
        edit(&p, &q, |s, g| {
            if s == LAYERS {
                set(g, 1, "material", Data::String(name.clone()));
            }
        });
        let rep = apply(&mut m, &q);
        assert!(rep.applied && rep.rejected.is_empty(), "{tag}: {:?}", rep.rejected);
        assert_eq!(layer_material(&m), name, "{tag}");
        // export again: the book carries the text unchanged and reads back as no edit
        let p2 = export(&m, &format!("text_c_{tag}"));
        let mut wb: Xlsx<_> = open_workbook(&p2).unwrap();
        let rg = wb.worksheet_range(LAYERS).unwrap();
        let hc = rg
            .rows()
            .next()
            .unwrap()
            .iter()
            .position(|d| d.to_string() == "material")
            .unwrap();
        assert_eq!(rg.rows().nth(1).unwrap()[hc].to_string(), name, "{tag}");
        let rep = apply(&mut m, &p2);
        assert!(rep.applied && rep.changes.is_empty(), "{tag}: {:?}", rep.changes);
        assert_eq!(layer_material(&m), name, "{tag}");
    }
}

#[test]
fn all_rows_removed_delete_off_noop() {
    let mut m = model();
    let p = export(&m, "norows_a");
    let before = walls_snapshot(&m);
    let q = tmp("edge_norows_b");
    edit(&p, &q, |s, g| {
        if s == WALLS {
            g.truncate(1);
        }
    });
    let rep = apply(&mut m, &q);
    assert!(rep.applied, "{rep:?}");
    assert!(rep.removed.iter().all(|r| r.sheet != WALLS), "{rep:?}");
    assert_eq!(rep.sheet_issues, [SheetIssue::new(WALLS, SheetIssueKind::Empty, 4)]);
    assert!(rep.edits.is_empty() && rep.changes.is_empty(), "{rep:?}");
    assert_eq!(walls_snapshot(&m), before);
}

/// Two levels and two wall types; walls 1-2 on L1/ext, wall 3 on L2/int.
fn model2() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let layers = [LayerSpec {
        material: "brick".into(),
        thickness: 0.2,
    }];
    let ext = m.add_wall_type("ext", &layers).unwrap();
    let int = m.add_wall_type("int", &layers).unwrap();
    for (i, (lv, t)) in [(l1, ext), (l1, ext), (l2, int)].into_iter().enumerate() {
        let w = WallSpec {
            a: (0., i as f64 * 3.),
            b: (5., i as f64 * 3.),
            level: lv,
            base_offset: 0.0,
            height: 2.5,
            wall_type: t,
            offset0: None,
        };
        m.add_wall(&w).unwrap();
    }
    m
}

/// (level name, type name) of every wall, by wall id.
fn wall_refs(m: &Model) -> Vec<(String, String)> {
    let mut st = m
        .sql()
        .prepare(
            "SELECT l.name, t.name FROM wall w JOIN level l ON l.id=w.level \
             JOIN wall_type t ON t.id=w.wtype ORDER BY w.id",
        )
        .unwrap();
    let rows = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn names(m: &Model, table: &str) -> Vec<String> {
    let mut st = m
        .sql()
        .prepare(&format!("SELECT name FROM {table} ORDER BY id"))
        .unwrap();
    let rows = st.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.map(|r| r.unwrap()).collect()
}

#[test]
fn rename_level_and_walls_follow() {
    let mut m = model2();
    let p = export(&m, "rnl_a");
    let q = tmp("edge_rnl_b");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            let r = row_of(g, 1);
            set(g, r, "name", Data::String("Ground".into()));
        }
    });
    // walls 1-2 keep the old level name, wall 3 already names the new one
    let rep = apply(&mut m, &q);
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(names(&m, "level"), vec!["Ground", "L2"]);
    let refs = wall_refs(&m);
    assert_eq!(refs.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["Ground", "Ground", "L2"]);
    assert_eq!(rep.changes.iter().filter(|c| c.sheet == WALLS).count(), 0, "{:?}", rep.changes);

    // a wall row that moves to the renamed level by its new name
    let p = export(&m, "rnl_c");
    let q = tmp("edge_rnl_d");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            let r = row_of(g, 2);
            set(g, r, "name", Data::String("Roof".into()));
        }
        if sh == WALLS {
            let r = row_of(g, 1);
            set(g, r, "level", Data::String("Roof".into()));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    let refs = wall_refs(&m);
    assert_eq!(refs.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["Roof", "Ground", "Roof"]);
}

#[test]
fn rename_type_and_instances_follow() {
    let mut m = model2();
    let p = export(&m, "rnt_a");
    let q = tmp("edge_rnt_b");
    edit(&p, &q, |sh, g| {
        if sh == TYPES {
            let r = row_of(g, 1);
            set(g, r, "name", Data::String("outer".into()));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(names(&m, "wall_type"), vec!["outer", "int"]);
    let refs = wall_refs(&m);
    assert_eq!(refs.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(), ["outer", "outer", "int"]);
    assert_eq!(rep.changes.iter().filter(|c| c.sheet == WALLS).count(), 0, "{:?}", rep.changes);
}

#[test]
fn wall_moves_to_another_level_in_a_book_that_renames_its_level() {
    let mut m = model2();
    let p = export(&m, "rmv_a");
    let q = tmp("edge_rmv_b");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            let r = row_of(g, 1);
            set(g, r, "name", Data::String("Ground".into()));
        }
        if sh == WALLS {
            let r = row_of(g, 1);
            set(g, r, "level", Data::String("L2".into()));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(rep.applied && rep.rejected.is_empty() && rep.conflicts.is_empty(), "{rep:?}");
    assert_eq!(names(&m, "level"), vec!["Ground", "L2"]);
    let moved: String = m
        .sql()
        .query_row(
            "SELECT level.name FROM wall JOIN level ON level.id = wall.level ORDER BY wall.id",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(moved, "L2");
}

#[test]
fn unknown_level_still_reported() {
    let mut m = model2();
    let p = export(&m, "unl_a");
    let q = tmp("edge_unl_b");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            let r = row_of(g, 1);
            set(g, r, "name", Data::String("Ground".into()));
        }
        if sh == WALLS {
            let r = row_of(g, 2);
            set(g, r, "level", Data::String("Basement".into()));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(
        rep.rejected
            .iter()
            .any(|r| r.sheet == WALLS && r.reason.contains("unknown level 'Basement'")),
        "{rep:?}"
    );
    // the rename and the other walls still apply; the bad row stays as it was
    assert_eq!(names(&m, "level"), vec!["Ground", "L2"]);
    assert_eq!(wall_refs(&m)[1].0, "Ground");
}

#[test]
fn rename_to_existing_name_reported() {
    let mut m = model2();
    let p = export(&m, "dupl_a");
    let before = (names(&m, "level"), wall_refs(&m), walls_snapshot(&m));
    let q = tmp("edge_dupl_b");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            let r = row_of(g, 1);
            set(g, r, "name", Data::String("L2".into()));
        }
    });
    let rep = apply(&mut m, &q);
    assert_eq!(rep.blocking(), 0, "{rep:?}");
    assert!(
        rep.rejected.iter().any(|r| r.reason.contains("duplicate level name 'L2'")),
        "{rep:?}"
    );
    assert_eq!((names(&m, "level"), wall_refs(&m), walls_snapshot(&m)), before);
}

#[test]
fn old_name_reused_by_new_level_not_guessed() {
    let mut m = model2();
    let p = export(&m, "reuse_a");
    let q = tmp("edge_reuse_b");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            let r = row_of(g, 1);
            set(g, r, "name", Data::String("Ground".into()));
            g.push(vec![Data::Empty; g[0].len()]);
            let n = g.len() - 1;
            set(g, n, "name", Data::String("L1".into()));
            set(g, n, "elevation [m]", Data::Float(6.0));
        }
        if sh == WALLS {
            let r = row_of(g, 3);
            set(g, r, "level", Data::String("L1".into()));
        }
    });
    let rep = apply(&mut m, &q);
    // 'L1' is now the new level, not yet in the model: the row is not moved to old L1
    assert!(
        rep.rejected
            .iter()
            .any(|r| r.sheet == WALLS && r.reason.contains("unknown level 'L1'")),
        "{rep:?}"
    );
    assert_eq!(wall_refs(&m)[2].0, "L2");
}

fn new_level_row(g: &mut Grid, name: &str, elevation: Data) {
    g.push(vec![Data::Empty; g[0].len()]);
    let n = g.len() - 1;
    set(g, n, "name", Data::String(name.into()));
    set(g, n, "elevation [m]", elevation);
}

#[test]
fn rejected_rename_frees_old_name() {
    let mut m = model2();
    let p = export(&m, "rrf_a");
    let q = tmp("edge_rrf_b");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            // the rename row is rejected (a date refuses its whole row): the model keeps "L1",
            // "Ground" stays free
            let r = row_of(g, 1);
            set(g, r, "name", Data::String("Ground".into()));
            let d = calamine::ExcelDateTime::new(
                45000.0,
                calamine::ExcelDateTimeType::DateTime,
                false,
            );
            set(g, r, "elevation [m]", Data::DateTime(d));
            new_level_row(g, "Ground", Data::Float(9.0));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(
        !rep.rejected.iter().any(|r| r.reason.contains("duplicate level name")),
        "{rep:?}"
    );
    assert_eq!(names(&m, "level"), vec!["L1", "L2", "Ground"]);
}

#[test]
fn bad_elevation_alone_does_not_cancel_a_duplicate_rename() {
    let mut m = model2();
    let p = export(&m, "rrd_a");
    let q = tmp("edge_rrd_b");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            // a bad cell skips alone, so the rename still counts and the two names collide
            let r = row_of(g, 1);
            set(g, r, "name", Data::String("Ground".into()));
            set(g, r, "elevation [m]", Data::String("abc".into()));
            new_level_row(g, "Ground", Data::Float(9.0));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(
        rep.rejected.iter().any(|r| r.reason.contains("duplicate level name")),
        "{rep:?}"
    );
    assert_eq!(names(&m, "level"), vec!["L1", "L2"]);
}

#[test]
fn rejected_new_level_not_in_names() {
    let mut m = model2();
    let p = export(&m, "rnn_a");
    let q = tmp("edge_rnn_b");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            new_level_row(g, "Attic", Data::String("abc".into()));
            new_level_row(g, "Attic", Data::Float(9.0));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(
        !rep.rejected.iter().any(|r| r.reason.contains("duplicate level name")),
        "{rep:?}"
    );
    assert_eq!(names(&m, "level"), vec!["L1", "L2", "Attic"]);

    // a rejected new level cannot be named by a wall row
    let mut m = model2();
    let q = tmp("edge_rnn_c");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            new_level_row(g, "Attic", Data::String("abc".into()));
        }
        if sh == WALLS {
            let r = row_of(g, 3);
            set(g, r, "level", Data::String("Attic".into()));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(
        rep.rejected
            .iter()
            .any(|r| r.sheet == WALLS && r.reason.contains("unknown level 'Attic'")),
        "{rep:?}"
    );
    assert_eq!(names(&m, "level"), vec!["L1", "L2"]);
}

#[test]
fn wall_on_level_created_in_same_book() {
    let mut m = model2();
    let p = export(&m, "wls_a");
    let q = tmp("edge_wls_b");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            new_level_row(g, "Roof", Data::Float(6.0));
        }
        if sh == WALLS {
            let r = row_of(g, 3);
            set(g, r, "level", Data::String("Roof".into()));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(names(&m, "level"), vec!["L1", "L2", "Roof"]);
    assert_eq!(wall_refs(&m)[2].0, "Roof");
}

#[test]
fn wall_cell_old_name_of_renamed_level() {
    let mut m = model2();
    let p = export(&m, "wco_a");
    let q = tmp("edge_wco_b");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            let r = row_of(g, 1);
            set(g, r, "name", Data::String("Ground".into()));
        }
        if sh == WALLS {
            let r = row_of(g, 3);
            set(g, r, "level", Data::String("L1".into()));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(wall_refs(&m)[2].0, "Ground");
}

// ---- formula cells whose result was never calculated ----

fn no_calc(rep: &Report) -> Vec<&Rejection> {
    rep.rejected.iter().filter(|r| r.reason.contains("has no calculated value")).collect()
}

/// Address of a cell in a column before Z (0-based row and column as in the grids).
fn addr(c: u16, r: u32) -> String {
    format!("{}{}", (b'A' + c as u8) as char, r + 1)
}

/// Book with formulas (wall id, expression, cached result) in the height column of the walls.
fn wall_formulas(m: &Model, tag: &str, cells: &[(i64, &str, &str)]) -> (PathBuf, u16) {
    let p = export(m, &format!("{tag}_a"));
    let g: Grid = {
        let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
        wb.worksheet_range(WALLS).unwrap().rows().map(|r| r.to_vec()).collect()
    };
    let c = col(&g, "height [m]") as u16;
    let f: Vec<(&str, u32, u16, &str, &str)> =
        cells.iter().map(|(id, e, res)| (WALLS, row_of(&g, *id) as u32, c, *e, *res)).collect();
    let q = tmp(&format!("edge_{tag}_b"));
    rewrite(&p, &q, &[], |_, _| {}, &f);
    (q, c)
}

/// Copies a book, `f` edits the xml of every worksheet.
fn patch_sheets(src: &Path, dst: &Path, f: impl Fn(&mut String)) {
    use std::io::{Read, Write};
    let mut z = zip::ZipArchive::new(std::fs::File::open(src).unwrap()).unwrap();
    let mut out = zip::ZipWriter::new(std::fs::File::create(dst).unwrap());
    for i in 0..z.len() {
        let mut e = z.by_index(i).unwrap();
        let name = e.name().unwrap().into_owned();
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).unwrap();
        if name.starts_with("xl/worksheets/sheet") {
            let mut s = String::from_utf8(buf).unwrap();
            f(&mut s);
            buf = s.into_bytes();
        }
        out.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        out.write_all(&buf).unwrap();
    }
    out.finish().unwrap();
}

/// Replaces the `<f>` element of the formula cell `at` (nothing happens on sheets without it).
fn patch_formula(s: &mut String, at: &str, new_f: &str) {
    let Some(c) = s.find(&format!("<c r=\"{at}\"")) else {
        return;
    };
    let end = s[c..].find("</c>").map_or(s.len(), |k| k + c);
    let Some(a) = s[c..end].find("<f>").map(|k| k + c) else {
        return;
    };
    let Some(b) = s[a..end].find("</f>").map(|k| k + a + 4) else {
        return;
    };
    s.replace_range(a..b, new_f);
}

fn level_name(m: &Model) -> String {
    m.sql().query_row("SELECT name FROM level WHERE id=1", [], |r| r.get::<_, String>(0)).unwrap()
}

#[test]
fn uncalculated_number_formula_is_refused_not_blank() {
    let mut m = model();
    let before = walls_snapshot(&m);
    let (q, c) = wall_formulas(&m, "nc_num", &[(1, "=2+3", "")]);
    let rep = apply(&mut m, &q);
    let bad = no_calc(&rep);
    assert_eq!(bad.len(), 1, "{rep:?}");
    assert_eq!(bad[0].sheet, WALLS);
    assert_eq!(rep.blocking(), 0, "{rep:?}");
    assert_eq!(bad[0].address(), cell_ref(WALLS, 2, c as u32 + 1));
    assert!(bad[0].reason.contains(&cell_ref(WALLS, 2, c as u32 + 1)), "{}", bad[0].reason);
    assert!(bad[0].reason.contains("=2+3"), "{}", bad[0].reason);
    assert!(rep.changes.iter().all(|ch| ch.sheet != WALLS), "{:?}", rep.changes);
    assert_eq!(walls_snapshot(&m), before, "model value must stay");
}

#[test]
fn uncalculated_formula_does_not_stop_the_other_cells() {
    let mut m = model();
    let (q, _) = wall_formulas(&m, "nc_mix", &[(1, "=2+3", ""), (2, "=2+3", "5")]);
    let before = heights(&m);
    let rep = apply(&mut m, &q);
    assert!(rep.applied, "{rep:?}");
    assert_eq!(no_calc(&rep).len(), 1, "{rep:?}");
    // the calculated one is listed as a formula, not applied
    assert_eq!(formula_left(&rep), 1, "{rep:?}");
    assert_eq!(heights(&m), before);
}

#[test]
fn cached_bool_and_error_formulas_are_not_taken_for_uncalculated() {
    let mut m = model();
    let before = heights(&m);
    let (q, _) = wall_formulas(&m, "nc_be", &[(1, "=TRUE", "TRUE"), (2, "=1/0", "#DIV/0!")]);
    let rep = apply(&mut m, &q);
    assert!(no_calc(&rep).is_empty(), "{rep:?}");
    assert_eq!(heights(&m), before, "{rep:?}");
}

#[test]
fn empty_cell_is_not_a_formula() {
    let mut m = model();
    let p = export(&m, "nc_empty_a");
    let q = tmp("edge_nc_empty_b");
    edit(&p, &q, |s, g| {
        if s == WALLS {
            let r = row_of(g, 1);
            set(g, r, "height [m]", Data::Empty);
            let r = row_of(g, 3);
            set(g, r, "height [m]", Data::Float(3.3));
        }
    });
    let rep = apply(&mut m, &q);
    assert!(rep.applied, "{rep:?}");
    assert!(no_calc(&rep).is_empty(), "{rep:?}");
    let h = heights(&m);
    assert!((h[0] - 2.5).abs() < 1e-9, "{h:?}");
    assert!((h[2] - 3.3).abs() < 1e-9, "{h:?}");
}

#[test]
fn uncalculated_text_formula_keeps_the_name() {
    let mut m = model();
    let p = export(&m, "nc_text_a");
    let g: Grid = {
        let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
        wb.worksheet_range(LEVELS).unwrap().rows().map(|r| r.to_vec()).collect()
    };
    let (r, c) = (row_of(&g, 1) as u32, col(&g, "name") as u16);
    // never calculated: refused, the level keeps its name
    let q = tmp("edge_nc_text_b");
    rewrite(&p, &q, &[], |_, _| {}, &[(LEVELS, r, c, "=\"Ground\"", "")]);
    let rep = apply(&mut m, &q);
    let bad = no_calc(&rep);
    assert_eq!(bad.len(), 1, "{rep:?}");
    assert_eq!(bad[0].sheet, LEVELS);
    assert!(bad[0].reason.contains("=\"Ground\""), "{}", bad[0].reason);
    assert_eq!(level_name(&m), "L1");
    // calculated: listed as a formula, the level keeps its name
    let q2 = tmp("edge_nc_text_c");
    rewrite(&p, &q2, &[], |_, _| {}, &[(LEVELS, r, c, "=\"Ground\"", "Ground")]);
    let rep = apply(&mut m, &q2);
    assert!(no_calc(&rep).is_empty(), "{rep:?}");
    assert_eq!(formula_left(&rep), 1, "{rep:?}");
    assert_eq!(level_name(&m), "L1");
}

#[test]
fn uncalculated_formula_in_the_id_column_stops_the_import() {
    let mut m = model();
    let before = walls_snapshot(&m);
    let p = export(&m, "nc_id_a");
    let g: Grid = {
        let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
        wb.worksheet_range(WALLS).unwrap().rows().map(|r| r.to_vec()).collect()
    };
    let (r, c) = (row_of(&g, 2) as u32, col(&g, "id") as u16);
    let q = tmp("edge_nc_id_b");
    rewrite(&p, &q, &[], |_, _| {}, &[(WALLS, r, c, "=1+1", "")]);
    let rep = apply(&mut m, &q);
    assert!(!rep.applied, "{rep:?}");
    assert!(rep.blocking() > 0 && no_calc(&rep).len() == 1, "{rep:?}");
    assert_eq!(walls_snapshot(&m), before);
}

#[test]
fn shared_formula_without_cache_is_refused_and_cached_ones_apply() {
    let mut m = model();
    // master with a cached result, the derived cell without one
    let (q, c) = wall_formulas(&m, "nc_sh", &[(1, "=2+3", "5"), (2, "=2+3", "")]);
    let q2 = tmp("edge_nc_sh_c");
    let (a1, a2) = (addr(c, 1), addr(c, 2));
    patch_sheets(&q, &q2, |s| {
        patch_formula(s, &a1, &format!("<f t=\"shared\" ref=\"{a1}:{a2}\" si=\"0\">2+3</f>"));
        patch_formula(s, &a2, "<f t=\"shared\" si=\"0\"/>");
    });
    let before = heights(&m);
    let rep = apply(&mut m, &q2);
    assert!(rep.applied, "{rep:?}");
    let bad = no_calc(&rep);
    assert_eq!(bad.len(), 1, "{rep:?}");
    assert_eq!(bad[0].address(), cell_ref(WALLS, 3, c as u32 + 1));
    assert!(bad[0].reason.contains("=2+3"), "{}", bad[0].reason);
    assert_eq!(formula_left(&rep), 1, "{rep:?}");
    assert_eq!(heights(&m), before);

    // both cached
    let mut m = model();
    let (q, c) = wall_formulas(&m, "nc_sh2", &[(1, "=2+3", "5"), (2, "=2+3", "6")]);
    let q2 = tmp("edge_nc_sh2_c");
    let (a1, a2) = (addr(c, 1), addr(c, 2));
    patch_sheets(&q, &q2, |s| {
        patch_formula(s, &a1, &format!("<f t=\"shared\" ref=\"{a1}:{a2}\" si=\"0\">2+3</f>"));
        patch_formula(s, &a2, "<f t=\"shared\" si=\"0\"/>");
    });
    let before = heights(&m);
    let rep = apply(&mut m, &q2);
    assert!(no_calc(&rep).is_empty(), "{rep:?}");
    assert_eq!(formula_left(&rep), 2, "{rep:?}");
    assert_eq!(heights(&m), before);
}

#[test]
fn array_formula_cached_is_listed_and_uncached_is_refused() {
    for (res, want) in [("5", Some(2.5)), ("", None)] {
        let mut m = model();
        let (q, c) = wall_formulas(&m, "nc_arr", &[(1, "=2+3", res)]);
        let q2 = tmp(&format!("edge_nc_arr_c{}", res.len()));
        let a1 = addr(c, 1);
        patch_sheets(&q, &q2, |s| {
            patch_formula(s, &a1, &format!("<f t=\"array\" ref=\"{a1}\">2+3</f>"));
        });
        let rep = apply(&mut m, &q2);
        let h = heights(&m);
        match want {
            Some(v) => {
                assert!(no_calc(&rep).is_empty() && formula_left(&rep) == 1, "{rep:?}");
                assert!((h[0] - v).abs() < 1e-9, "{h:?}");
            }
            None => {
                assert_eq!(no_calc(&rep).len(), 1, "{rep:?}");
                assert!((h[0] - 2.5).abs() < 1e-9, "{h:?}");
            }
        }
    }
}

#[test]
fn uncalculated_message_is_a_template_with_cell_and_formula() {
    let mut m = model();
    let (q, c) = wall_formulas(&m, "nc_msg", &[(3, "=SUM(1,2)", "")]);
    let rep = apply(&mut m, &q);
    let bad = no_calc(&rep);
    assert_eq!(bad.len(), 1, "{rep:?}");
    let args = super::msg::match_template(super::msg::NO_CALC.1, &bad[0].reason).expect("template");
    assert_eq!(args, vec![cell_ref(WALLS, 4, c as u32 + 1), "=SUM(1,2)".to_string()]);
    assert!(bad[0].reason.contains("let it calculate"), "{}", bad[0].reason);
}

#[test]
fn calculated_empty_text_is_a_value_not_uncalculated() {
    let mut m = model();
    let before = heights(&m);
    let (q, _) = wall_formulas(&m, "nc_ifempty", &[(1, "=IF(1>2,3,\"\")", "zz")]);
    let q2 = tmp("edge_nc_ifempty_c");
    // the spreadsheet cached "" (t="str" with an empty <v>)
    patch_sheets(&q, &q2, |s| *s = s.replace("<v>zz</v>", "<v></v>"));
    let rep = apply(&mut m, &q2);
    assert!(no_calc(&rep).is_empty(), "{rep:?}");
    assert_eq!(heights(&m), before, "{rep:?}");
}
