//! Re-exporting a book that carries a user layout is a fixed point: with an unchanged model the
//! second and third re-export write identical sheet XML and report no loss; one model edit then
//! changes only the cells of that element.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, WallSpec};
use rust_xlsxwriter::{Formula, Workbook, Worksheet};

use super::sheet_view::{read_book, View};
use super::tests::tmp;
use super::*;

type Grid = Vec<Vec<Data>>;

/// Walls on two levels (w1 and w3 on L1, w2 on L2) over one three-layer type.
fn model() -> (Model, [i64; 3]) {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let layers = [("brick", 0.25), ("wool", 0.1), ("plaster", 0.02)]
        .map(|(material, thickness)| LayerSpec { material: material.into(), thickness });
    let t = m.add_wall_type("ext", &layers).unwrap();
    let mut ids = [0; 3];
    for (i, (x, lv)) in [(0.0, l1), (3.0, l2), (6.0, l1)].into_iter().enumerate() {
        let w = WallSpec {
            a: (x, 0.0),
            b: (x + 2.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        ids[i] = m.add_wall(&w).unwrap();
    }
    (m, ids)
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

fn id_of(row: &[Data]) -> i64 {
    match row.first() {
        Some(Data::Float(x)) => *x as i64,
        Some(Data::Int(x)) => *x,
        d => panic!("no id: {d:?}"),
    }
}

fn header_row(g: &Grid, key: &str) -> usize {
    g.iter().position(|r| r.iter().any(|d| units::norm(&d.to_string()) == key)).expect("header row")
}

fn col_at(g: &Grid, hr: usize, key: &str) -> usize {
    g[hr].iter().position(|d| units::norm(&d.to_string()) == key).expect("column")
}

fn grid(p: &Path, sheet: &str) -> Grid {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.worksheet_range(sheet).unwrap().rows().map(|r| r.to_vec()).collect()
}

/// The first export as a user left it: Walls sorted by level, a column hidden, a custom width,
/// a hidden row, own formula and text columns, frozen panes, a filter and everything right of
/// the table hidden; Layers with a note column and a hidden row.
fn user_book(src: &Path, tag: &str) -> PathBuf {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for n in wb.sheet_names() {
        let mut g: Grid = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
        let hr = match n.as_str() {
            WALLS => header_row(&g, "id"),
            LAYERS => header_row(&g, "type_id"),
            _ => 0,
        };
        if n == WALLS {
            let lv = col_at(&g, hr, "level");
            g[hr + 1..].sort_by_key(|r| r[lv].to_string());
        }
        let ws = out.add_worksheet();
        ws.set_name(&n).unwrap();
        for (i, row) in g.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                put(ws, i as u32, j as u16, d);
            }
        }
        let nc = g.iter().map(|r| r.len()).max().unwrap_or(0) as u16;
        let last = g.len() as u32 - 1;
        if n == WALLS {
            ws.write_string(hr as u32, nc, "calc").unwrap();
            ws.write_string(hr as u32, nc + 1, "memo").unwrap();
            for (i, row) in g.iter().enumerate().skip(hr + 1) {
                let id = id_of(row);
                let f = Formula::new(format!("=A{}*2", i + 1)).set_result(format!("{}", id * 2));
                ws.write_formula(i as u32, nc, f).unwrap();
                ws.write_string(i as u32, nc + 1, format!("m{id}")).unwrap();
            }
            ws.set_column_hidden(col_at(&g, hr, "offset0") as u16).unwrap();
            ws.set_column_width(col_at(&g, hr, "base_offset") as u16, 25.0).unwrap();
            ws.set_column_width(nc, 14.0).unwrap();
            ws.set_column_range_hidden(nc + 3, 16_383).unwrap();
            ws.set_row_hidden(hr as u32 + 2).unwrap();
            ws.set_freeze_panes(hr as u32 + 1, 2).unwrap();
            ws.autofilter(hr as u32, 0, last, nc + 1).unwrap();
        } else if n == LAYERS {
            ws.write_string(hr as u32, nc, "memo").unwrap();
            for (i, row) in g.iter().enumerate().skip(hr + 1) {
                let (t, p) = (col_at(&g, hr, "type_id"), col_at(&g, hr, "pos"));
                let key = format!("n{}_{}", id_of(&row[t..]), id_of(&row[p..]));
                ws.write_string(i as u32, nc, key).unwrap();
            }
            ws.set_column_width(nc, 30.0).unwrap();
            ws.set_row_hidden(hr as u32 + 2).unwrap();
        }
    }
    let q = tmp(tag);
    out.save(&q).unwrap();
    q
}

fn reexport(m: &Model, old: &Path, tag: &str) -> (PathBuf, Vec<String>) {
    let new = tmp(tag);
    export_xlsx_families_from(m, old, &new, Units::Metric, &[]).unwrap();
    let lost = reexport_losses(old, &new, &[], 0).unwrap();
    (new, lost)
}

fn xmls(p: &Path) -> HashMap<String, String> {
    super::loss::sheet_xmls(p, |n| n == WALLS || n == LAYERS)
}

fn same_view(a: &View, b: &View, what: &str) {
    assert_eq!(a.order, b.order, "{what}: order");
    assert_eq!(a.hidden_rows, b.hidden_rows, "{what}: hidden rows");
    assert_eq!(a.heights, b.heights, "{what}: heights");
    assert_eq!(a.head_height, b.head_height, "{what}: header height");
    // every user width and hidden flag survives; a column the user left alone gets the export's
    // default width (the fixture copy drops the widths a real exported book carries)
    for (k, cv) in &b.cols {
        let got = a.cols.get(k);
        assert_eq!(got.map(|c| c.hidden), Some(cv.hidden), "{what}: column {k} hidden");
        if cv.width.is_some() {
            assert_eq!(got.and_then(|c| c.width), cv.width, "{what}: column {k} width");
        }
    }
    let hidden = |v: &View| {
        let mut h: Vec<String> =
            v.cols.iter().filter(|c| c.1.hidden).map(|c| c.0.clone()).collect();
        h.sort();
        h
    };
    assert_eq!(hidden(a), hidden(b), "{what}: hidden columns");
    assert_eq!(a.hidden_tail, b.hidden_tail, "{what}: hidden tail");
}

/// The user's own cells of Walls as they stand in `p`: (id, calc, memo) in sheet order.
fn own_cells(p: &Path) -> Vec<(i64, Data, Data)> {
    let g = grid(p, WALLS);
    let hr = header_row(&g, "id");
    let (c, t) = (col_at(&g, hr, "calc"), col_at(&g, hr, "memo"));
    g[hr + 1..].iter().map(|r| (id_of(r), r[c].clone(), r[t].clone())).collect()
}

#[test]
fn user_layout_is_a_fixed_point_and_one_edit_changes_only_its_cells() {
    let (m, [w1, w2, w3]) = model();
    let src = tmp("rfp_src");
    let _ = std::fs::remove_file(&src);
    export_xlsx(&m, &src).unwrap();
    let old = user_book(&src, "rfp_old");
    let v0 = read_book(&old);
    assert_eq!(v0[WALLS].order, vec![w1, w3, w2], "the fixture is sorted by level");
    assert_eq!(v0[WALLS].hidden_tail, Some(1));

    let (b1, l1) = reexport(&m, &old, "rfp_b1");
    let (b2, l2) = reexport(&m, &b1, "rfp_b2");
    let (b3, l3) = reexport(&m, &b2, "rfp_b3");
    for (i, l) in [l1, l2, l3].iter().enumerate() {
        assert!(l.is_empty(), "round {}: {l:?}", i + 1);
    }

    // sheet XML of the user-layout parts: identical from the second re-export on
    let (x2, x3) = (xmls(&b2), xmls(&b3));
    for s in [WALLS, LAYERS] {
        assert_eq!(x2[s], x3[s], "sheet XML of {s} differs between re-exports 2 and 3");
    }
    assert!(x3[WALLS].contains("<pane"), "frozen panes");
    assert!(x3[WALLS].contains("<autoFilter"), "filter range");
    assert!(x3[WALLS].matches("<f>").count() >= 3, "own formulas stay formulas");

    // the arrangement itself is carried through every round
    for (n, b) in [("1", &b1), ("2", &b2), ("3", &b3)] {
        let v = read_book(b);
        same_view(&v[WALLS], &v0[WALLS], &format!("Walls round {n}"));
        same_view(&v[LAYERS], &v0[LAYERS], &format!("Layers round {n}"));
        assert_eq!(own_cells(b), own_cells(&old), "own columns round {n}");
    }
    assert_eq!(grid(&b2, LAYERS), grid(&b3, LAYERS));
    assert_eq!(grid(&b2, WALLS), grid(&b3, WALLS));

    // one model edit: only the cells of that wall move
    m.sql().execute("UPDATE wall SET height=3.5 WHERE id=?1", [w1]).unwrap();
    let (b4, l4) = reexport(&m, &b3, "rfp_b4");
    assert!(l4.is_empty(), "{l4:?}");
    let (g3, g4) = (grid(&b3, WALLS), grid(&b4, WALLS));
    assert_eq!(g3.len(), g4.len());
    let hr = header_row(&g3, "id");
    let (hc, calc, memo) = (col_at(&g3, hr, "height"), col_at(&g3, hr, "calc"), col_at(&g3, hr, "memo"));
    let mut changed = Vec::new();
    for (i, (r3, r4)) in g3.iter().zip(&g4).enumerate() {
        assert_eq!(r3.len(), r4.len());
        for (j, (a, b)) in r3.iter().zip(r4).enumerate() {
            if a != b {
                changed.push((i, j));
            }
        }
    }
    assert!(changed.contains(&(hr + 1, hc)), "the edited height is written: {changed:?}");
    assert!(changed.iter().all(|c| c.0 == hr + 1), "only the edited wall's row changes: {changed:?}");
    assert!(changed.iter().all(|c| c.1 != calc && c.1 != memo), "own columns stay: {changed:?}");
    assert_eq!(id_of(&g4[hr + 1]), w1);
    assert_eq!(grid(&b3, LAYERS), grid(&b4, LAYERS));
    assert_eq!(xmls(&b3)[LAYERS], xmls(&b4)[LAYERS]);
    let (v3, v4) = (read_book(&b3), read_book(&b4));
    same_view(&v4[WALLS], &v3[WALLS], "Walls after the edit");
    same_view(&v4[LAYERS], &v3[LAYERS], "Layers after the edit");
    assert_eq!(own_cells(&b4), own_cells(&b3));
}
