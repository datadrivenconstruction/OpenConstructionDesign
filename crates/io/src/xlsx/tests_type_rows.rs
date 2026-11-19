// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rows that change a type are told apart from element rows, and each type counts once.

use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, WallSpec};
use rust_xlsxwriter::Workbook;

use super::super::{export_xlsx, import_xlsx_with, layer_key, ImportOptions};
use super::*;

type Grid = Vec<Vec<Data>>;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("ddc_xlsx_tests");
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{}_trw_{name}.xlsx", std::process::id()))
}

/// Three walls of type `ext`, a spare wall type and a slab type nobody uses.
fn model() -> (Model, i64, i64, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let brick = LayerSpec { material: "brick".into(), thickness: 0.25 };
    let t = m.add_wall_type("ext", &[brick.clone()]).unwrap();
    let spare = m.add_wall_type("spare", &[brick]).unwrap();
    let floor = LayerSpec { material: "concrete".into(), thickness: 0.2 };
    let st = m.add_slab_type("floor", &[floor]).unwrap();
    for x in [0.0, 3.0, 6.0] {
        let w = WallSpec {
            a: (x, 0.0),
            b: (x + 2.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        m.add_wall(&w).unwrap();
    }
    (m, t, spare, st)
}

/// Copies a book cell by cell, `f` edits the grids.
fn edit(src: &Path, name: &str, f: impl Fn(&str, &mut Grid)) -> PathBuf {
    let dst = tmp(name);
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for sheet in wb.sheet_names() {
        let mut g: Grid = wb.worksheet_range(&sheet).unwrap().rows().map(|r| r.to_vec()).collect();
        f(&sheet, &mut g);
        let ws = out.add_worksheet();
        ws.set_name(&sheet).unwrap();
        for (i, row) in g.iter().enumerate() {
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
    out.save(&dst).unwrap();
    dst
}

fn col(g: &Grid, header: &str) -> usize {
    g[0].iter()
        .position(|d| d.to_string() == header)
        .unwrap_or_else(|| panic!("no column {header}: {:?}", g[0]))
}

/// Grid row whose `key` column holds `id`.
fn row_of(g: &Grid, key: &str, id: i64) -> usize {
    let c = col(g, key);
    (1..g.len())
        .find(|r| g[*r].get(c).is_some_and(|d| d.to_string() == id.to_string()))
        .unwrap_or_else(|| panic!("no row {key} = {id}"))
}

fn preview(m: &mut Model, p: &Path) -> Report {
    let o = ImportOptions { apply: false, delete_missing: false, new_params: false };
    import_xlsx_with(m, p, o).unwrap()
}

#[test]
fn layer_rows_name_their_type_and_element_sheets_name_none() {
    assert_eq!(type_row("Layers", layer_key(7, 2)), Some(("wall_type", 7)));
    assert_eq!(type_row("SlabLayers", layer_key(12, 0)), Some(("slab_type", 12)));
    assert_eq!(type_row("WallTypes", 7), Some(("wall_type", 7)));
    assert_eq!(type_row("SlabTypes", 4), Some(("slab_type", 4)));
    assert_eq!(type_row("MemberTypes", 9), Some(("member_type", 9)));
    for s in ["Walls", "Slabs", "Columns", "Materials", "Levels"] {
        assert_eq!(type_row(s, 1), None, "{s}");
    }
}

#[test]
fn a_type_renamed_with_two_layer_edits_counts_once() {
    let (mut m, t, _, st) = model();
    let p = tmp("src");
    export_xlsx(&m, &p).unwrap();
    let q = edit(&p, "edit", move |s, g| match s {
        "WallTypes" => {
            let (r, c) = (row_of(g, "id", t), col(g, "name"));
            g[r][c] = Data::String("ext 2".into());
        }
        "Layers" => {
            let (r, c) = (row_of(g, "type_id", t), col(g, "thickness [m]"));
            g[r][c] = Data::Float(0.3);
            let mc = col(g, "material");
            g[r][mc] = Data::String("block".into());
        }
        "SlabLayers" => {
            let (r, c) = (row_of(g, "type_id", st), col(g, "thickness [m]"));
            g[r][c] = Data::Float(0.25);
        }
        _ => {}
    });
    let rep = preview(&mut m, &q);
    assert!(rep.rejected.iter().all(|r| !r.blocks()), "{:?}", rep.rejected);
    assert!(rep.changes.len() >= 4, "{:?}", rep.changes);
    let mut want = vec![("slab_type", st), ("wall_type", t)];
    want.sort_unstable();
    assert_eq!(changed_types(&rep), want);
}

#[test]
fn an_element_edit_changes_no_type() {
    let (mut m, _, spare, _) = model();
    let p = tmp("walls_src");
    export_xlsx(&m, &p).unwrap();
    let q = edit(&p, "walls", |s, g| {
        if s == "Walls" {
            let c = col(g, "height [m]");
            g[1][c] = Data::Float(3.5);
        }
    });
    let rep = preview(&mut m, &q);
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert!(changed_types(&rep).is_empty());
    // a type nobody uses is still a changed type; the count of its users is zero elsewhere
    let q = edit(&p, "spare", move |s, g| {
        if s == "Layers" {
            let (r, c) = (row_of(g, "type_id", spare), col(g, "thickness [m]"));
            g[r][c] = Data::Float(0.4);
        }
    });
    assert_eq!(changed_types(&preview(&mut m, &q)), vec![("wall_type", spare)]);
}
