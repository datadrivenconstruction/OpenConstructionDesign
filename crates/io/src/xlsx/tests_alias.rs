// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A main sheet the user renamed: found by its header row, offered to the user, read only for
//! the import the user chose it for; the workbook is never written.

use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, WallSpec};
use rust_xlsxwriter::Workbook;

use super::*;

type Grid = Vec<Vec<Data>>;

/// Name a user would type in a Russian-language office.
const RU_WALLS: &str = "\u{421}\u{442}\u{435}\u{43d}\u{44b}";
const RU_WALLS_2: &str = "\u{421}\u{442}\u{435}\u{43d}\u{44b} 2";
const H: &str = "height [m]";

fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = LayerSpec { material: "brick".into(), thickness: 0.2 };
    let t = m.add_wall_type("ext", &[layer]).unwrap();
    for (a, b) in [((0., 0.), (5., 0.)), ((5., 0.), (5., 4.))] {
        let w = WallSpec { a, b, level: lv, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None };
        m.add_wall(&w).unwrap();
    }
    m
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("ddc_xlsx_tests");
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{}_alias_{name}.xlsx", std::process::id()))
}

fn exported(m: &Model, name: &str) -> PathBuf {
    let p = tmp(name);
    export_xlsx(m, &p).unwrap();
    p
}

fn grids(src: &Path) -> Vec<(String, Grid)> {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    wb.sheet_names()
        .into_iter()
        .map(|n| {
            let g: Grid = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
            (n, g)
        })
        .collect()
}

fn save(dst: &Path, sheets: &[(String, Grid)]) {
    let mut out = Workbook::new();
    for (name, grid) in sheets {
        let ws = out.add_worksheet();
        ws.set_name(name).unwrap();
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                    Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(dst).unwrap();
}

fn col(g: &Grid, header: &str) -> usize {
    g[0].iter().position(|d| d.to_string() == header).unwrap_or_else(|| panic!("no column {header}"))
}

/// Book from the export with the walls sheet changed by `f` and called `walls_as`; the height of
/// wall 1 is 4.0.
fn book(m: &Model, tag: &str, walls_as: &str, f: impl Fn(&mut Grid)) -> PathBuf {
    let mut sheets = grids(&exported(m, &format!("{tag}_src")));
    for (n, g) in &mut sheets {
        if n.as_str() == WALLS {
            let c = col(g, H);
            g[1][c] = Data::Float(4.0);
            f(g);
            *n = walls_as.to_string();
        }
    }
    let q = tmp(&format!("{tag}_dst"));
    save(&q, &sheets);
    q
}

fn preview() -> ImportOptions {
    ImportOptions { apply: false, delete_missing: false, new_params: false }
}

fn alias_to(sheet: &str) -> KeyDecisions {
    let mut d = KeyDecisions::default();
    d.alias.insert(WALLS.into(), sheet.into());
    d
}

fn height(m: &Model, id: i64) -> f64 {
    m.sql().query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0)).unwrap()
}

fn found(rep: &Report) -> Vec<String> {
    rep.candidates
        .iter()
        .filter(|c| c.expected == WALLS)
        .flat_map(|c| c.found.iter().map(|f| f.sheet.clone()))
        .collect()
}

fn changes(rep: &Report) -> String {
    format!("{:?}", rep.changes)
}

#[test]
fn renamed_sheet_is_found_by_headers_and_not_read() {
    let mut m = model();
    let q = book(&m, "found", RU_WALLS, |_| {});
    let rep = import_xlsx_keyed(&mut m, &q, preview(), &KeyDecisions::default()).unwrap();
    assert_eq!(found(&rep), vec![RU_WALLS.to_string()]);
    assert_eq!(rep.candidates[0].found[0].score, 100);
    assert!(rep.changes.is_empty(), "nothing is read before the user picks: {:?}", rep.changes);
}

#[test]
fn alias_read_gives_the_same_report_as_the_original_name() {
    let mut m = model();
    let orig = book(&m, "same_o", WALLS, |_| {});
    let want = import_xlsx_keyed(&mut m, &orig, preview(), &KeyDecisions::default()).unwrap();
    assert!(!want.changes.is_empty(), "the fixture changes a wall");
    let q = book(&m, "same_r", RU_WALLS, |_| {});
    let got = import_xlsx_keyed(&mut m, &q, preview(), &alias_to(RU_WALLS)).unwrap();
    assert_eq!(changes(&got), changes(&want));
    assert!(got.candidates.is_empty() && got.sheet_issues.is_empty(), "{got:?}");
    assert_eq!(got.rejected, want.rejected);
}

#[test]
fn alias_apply_edits_the_model_without_deleting_other_walls() {
    let mut m = model();
    let q = book(&m, "apply", RU_WALLS, |_| {});
    let o = ImportOptions { apply: true, delete_missing: false, new_params: false };
    let rep = import_xlsx_keyed(&mut m, &q, o, &alias_to(RU_WALLS)).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!((height(&m, 1) - 4.0).abs() < 1e-12);
    assert!((height(&m, 2) - 3.0).abs() < 1e-12, "untouched wall stays");
}

#[test]
fn shuffled_header_order_is_found_and_read() {
    let mut m = model();
    let rev = |g: &mut Grid| g.iter_mut().for_each(|r| r.reverse());
    let q = book(&m, "shuffle", RU_WALLS, rev);
    let rep = import_xlsx_keyed(&mut m, &q, preview(), &KeyDecisions::default()).unwrap();
    assert_eq!(found(&rep), vec![RU_WALLS.to_string()]);
    let rep = import_xlsx_keyed(&mut m, &q, preview(), &alias_to(RU_WALLS)).unwrap();
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert_eq!(rep.changes[0].sheet, WALLS);
}

#[test]
fn extra_and_dropped_columns_still_match() {
    let mut m = model();
    let extra = |g: &mut Grid| {
        for (i, r) in g.iter_mut().enumerate() {
            r.push(if i == 0 { Data::String("my note".into()) } else { Data::String("x".into()) });
            r.push(if i == 0 { Data::String("my tag".into()) } else { Data::Empty });
        }
    };
    let q = book(&m, "extra", RU_WALLS, extra);
    let rep = import_xlsx_keyed(&mut m, &q, preview(), &KeyDecisions::default()).unwrap();
    assert_eq!(found(&rep), vec![RU_WALLS.to_string()]);
    // two optional columns removed: 11 of 13 columns is above the threshold
    let fewer = |g: &mut Grid| {
        let drop: Vec<usize> = ["delete", "offset0 [m]"].iter().map(|h| col(g, h)).collect();
        for r in g.iter_mut() {
            let mut i = 0;
            r.retain(|_| {
                i += 1;
                !drop.contains(&(i - 1))
            });
        }
    };
    let q = book(&m, "fewer", RU_WALLS, fewer);
    let rep = import_xlsx_keyed(&mut m, &q, preview(), &KeyDecisions::default()).unwrap();
    assert_eq!(found(&rep), vec![RU_WALLS.to_string()]);
    assert!(rep.candidates[0].found[0].score >= 80);
}

#[test]
fn two_candidates_are_both_offered_and_none_is_read() {
    let mut m = model();
    let q = book(&m, "two_a", RU_WALLS, |_| {});
    let mut sheets = grids(&q);
    let copy = sheets.iter().find(|(n, _)| n.as_str() == RU_WALLS).unwrap().1.clone();
    sheets.push((RU_WALLS_2.into(), copy));
    let q2 = tmp("two_b");
    save(&q2, &sheets);
    let o = ImportOptions { apply: true, delete_missing: false, new_params: false };
    let rep = import_xlsx_keyed(&mut m, &q2, o, &KeyDecisions::default()).unwrap();
    let mut f = found(&rep);
    f.sort();
    assert_eq!(f, vec![RU_WALLS.to_string(), RU_WALLS_2.to_string()]);
    assert!(rep.changes.is_empty());
    assert!((height(&m, 1) - 3.0).abs() < 1e-12, "no sheet is picked on its own");
}

#[test]
fn unrelated_sheet_sharing_two_headers_is_not_offered() {
    let mut m = model();
    let q = book(&m, "unrel_a", WALLS, |_| {});
    let mut sheets = grids(&q);
    sheets.retain(|(n, _)| n.as_str() != WALLS);
    let other: Grid = vec![
        vec![Data::String("id".into()), Data::String("level".into()), Data::String("remarks".into())],
        vec![Data::Float(1.0), Data::String("L1".into()), Data::String("a".into())],
    ];
    sheets.push(("Notes".into(), other));
    let q2 = tmp("unrel_b");
    save(&q2, &sheets);
    let rep = import_xlsx_keyed(&mut m, &q2, preview(), &KeyDecisions::default()).unwrap();
    assert!(found(&rep).is_empty(), "{:?}", rep.candidates);
}

#[test]
fn the_choice_is_per_import_and_the_book_is_untouched() {
    let mut m = model();
    let q = book(&m, "once", RU_WALLS, |_| {});
    let before = std::fs::read(&q).unwrap();
    let dec = alias_to(RU_WALLS);
    let with = import_xlsx_keyed(&mut m, &q, preview(), &dec).unwrap();
    assert_eq!(with.changes.len(), 1);
    // the next import without the choice is back to asking
    let plain = import_xlsx(&mut m, &q).unwrap();
    assert!(plain.changes.is_empty());
    assert_eq!(found(&plain), vec![RU_WALLS.to_string()]);
    assert_eq!(std::fs::read(&q).unwrap(), before, "the workbook is never written");
}

#[test]
fn original_name_still_works_and_a_stale_alias_is_ignored() {
    let mut m = model();
    let q = book(&m, "orig", WALLS, |_| {});
    let plain = import_xlsx_keyed(&mut m, &q, preview(), &KeyDecisions::default()).unwrap();
    assert_eq!(plain.changes.len(), 1);
    assert!(plain.candidates.is_empty());
    let stale = import_xlsx_keyed(&mut m, &q, preview(), &alias_to("No such sheet")).unwrap();
    assert_eq!(changes(&stale), changes(&plain));
}

#[test]
fn a_picked_sheet_is_not_offered_for_another_known_sheet() {
    let mut m = model();
    let q = book(&m, "taken", RU_WALLS, |_| {});
    // other sheets that are missing from the book never list the sheet already read as Walls
    let rep = import_xlsx_keyed(&mut m, &q, preview(), &alias_to(RU_WALLS)).unwrap();
    assert!(rep.candidates.iter().all(|c| c.found.iter().all(|f| f.sheet != RU_WALLS)));
}

#[test]
fn copied_row_on_a_renamed_sheet_is_refused_and_decided_under_the_known_name() {
    let mut m = model();
    // wall 1 is 4.0 on its own row (sheet row 2), 5.0 on a copy at the end (sheet row 4)
    let copy = |g: &mut Grid| {
        let mut r = g[1].clone();
        r[col(g, H)] = Data::Float(5.0);
        g.push(r);
    };
    let q = book(&m, "dup", RU_WALLS, copy);
    let rep = import_xlsx_keyed(&mut m, &q, preview(), &alias_to(RU_WALLS)).unwrap();
    let refused: Vec<&Rejection> = rep.rejected.iter().filter(|r| dup_conflict_rows(&r.reason).is_some()).collect();
    assert_eq!(refused.len(), 2, "{:?}", rep.rejected);
    assert!(refused.iter().all(|r| r.sheet == WALLS), "decisions are keyed by the known name");
    assert_eq!(dup_conflict_rows(&refused[0].reason), Some((1, vec![2, 4])));
    // the copy becomes a new wall, the original row is then the edit of wall 1
    let mut dec = alias_to(RU_WALLS);
    dec.reject.insert((WALLS.to_string(), 4));
    let pre = import_xlsx_keyed(&mut m, &q, preview(), &dec).unwrap();
    let o = ImportOptions { apply: true, delete_missing: false, new_params: false };
    let rep = import_xlsx_keyed(&mut m, &q, o, &dec).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(format!("{:?}", rep.edits), format!("{:?}", pre.edits));
    assert!((height(&m, 1) - 4.0).abs() < 1e-12);
    assert!((height(&m, 2) - 3.0).abs() < 1e-12, "untouched wall stays");
    let n: i64 = m.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 3, "one new wall from the copy");
}
