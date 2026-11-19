//! Merged cells on import: data merges fill the covered cells, a merge down the id column is
//! reported and blocks deletes, a header merged across columns stops the import.

use std::path::PathBuf;

use calamine::{open_workbook, Data, Dimensions, Reader, Xlsx};
use ddc_model::{CeilingSpec, LayerSpec, Model, WallSpec};
use rust_xlsxwriter::{Format, Workbook};

use super::merge;
use super::tests::{col, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;

const SHEET: &str = "Ceilings";
const RING: [(f64, f64); 4] = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)];

fn get_h(m: &Model, id: i64) -> Result<f64, ddc_model::Error> {
    Ok(m.sql()
        .query_row("SELECT height FROM ceiling WHERE id=?1", [id], |r| r.get(0))?)
}

fn set_h(m: &mut Model, id: i64, v: f64) -> Result<(), ddc_model::Error> {
    let (level, slab_type): (i64, i64) = m.sql().query_row(
        "SELECT level,stype FROM ceiling WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let s = CeilingSpec { level, slab_type, height: v, outer: RING.to_vec(), holes: vec![] };
    m.update_ceiling(id, &s)
}

fn fams() -> Vec<FamilySheet> {
    vec![FamilySheet {
        sheet: SHEET,
        table: "ceiling",
        cols: vec![FamilyCol {
            key: "height",
            label: "Height".into(),
            dim: FamDim::Len,
            labels: vec![],
            lo: 0.0,
            hi: 100.0,
            get: get_h,
            set: set_h,
        }],
        delete: RowDelete::Allowed,
    }]
}

fn model() -> (Model, Vec<i64>) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = LayerSpec { material: "plaster".into(), thickness: 0.02 };
    let st = m.add_slab_type("ceiling", &[layer]).unwrap();
    let ids = [2.75, 2.6]
        .map(|h| {
            let s = CeilingSpec { level: lv, slab_type: st, height: h, outer: RING.to_vec(), holes: vec![] };
            m.add_ceiling(&s).unwrap()
        })
        .to_vec();
    (m, ids)
}

fn opts(delete_missing: bool) -> ImportOptions {
    ImportOptions { apply: true, delete_missing, new_params: false }
}

fn ceilings(m: &Model) -> i64 {
    m.sql().query_row("SELECT COUNT(*) FROM ceiling", [], |r| r.get(0)).unwrap()
}

/// Copies the book, lets `f` set cells of sheet `on` and return ranges to merge
/// as 0-based (first row, first col, last row, last col).
fn merged_book(
    src: &PathBuf,
    name: &str,
    on: &str,
    f: impl Fn(&mut Grid) -> Vec<(u32, u16, u32, u16)>,
) -> PathBuf {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for n in wb.sheet_names() {
        let mut grid: Grid = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
        let merges = if n == on { f(&mut grid) } else { vec![] };
        let ws = out.add_worksheet();
        ws.set_name(&n).unwrap();
        let put = |ws: &mut rust_xlsxwriter::Worksheet, i: u32, j: u16, d: &Data| {
            match d {
                Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                Data::Bool(b) => ws.write_boolean(i, j, *b).map(|_| ()),
                _ => Ok(()),
            }
            .unwrap();
        };
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                put(ws, i as u32, j as u16, d);
            }
        }
        for (r1, c1, r2, c2) in merges {
            ws.merge_range(r1, c1, r2, c2, "", &Format::new()).unwrap();
            put(ws, r1, c1, &grid[r1 as usize][c1 as usize]);
        }
    }
    let q = tmp(name);
    out.save(&q).unwrap();
    q
}

fn height_col(g: &Grid) -> usize {
    col(g, "Height [m]")
}

fn family_book(m: &Model, name: &str) -> PathBuf {
    let p = tmp(name);
    export_xlsx_families(m, &p, Units::Metric, &fams()).unwrap();
    p
}

#[test]
fn vertical_merge_in_data_column_fills_covered_cells() {
    let (mut m, ids) = model();
    let p = family_book(&m, "mrg_v_src");
    let q = merged_book(&p, "mrg_v", SHEET, |g| {
        let c = height_col(g);
        g[1][c] = Data::Float(3.1);
        vec![(1, c as u16, 2, c as u16)]
    });
    let rep = import_xlsx_families(&mut m, &q, opts(false), &fams()).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    for id in ids {
        assert!((get_h(&m, id).unwrap() - 3.1).abs() < 1e-12);
    }
}

#[test]
fn merge_over_id_column_is_reported_and_rows_left_out() {
    let (mut m, ids) = model();
    let p = family_book(&m, "mrg_id_src");
    let q = merged_book(&p, "mrg_id", SHEET, |g| {
        let c = height_col(g);
        g[1][c] = Data::Float(3.1);
        g[2][c] = Data::Float(3.3);
        vec![(1, 0, 2, 0)]
    });
    let rep = import_xlsx_families(&mut m, &q, opts(false), &fams()).unwrap();
    let want = fill_template(merge::ID_TPL, &["2..3"]);
    assert!(rep.ignored.iter().any(|i| i.reason == want), "{rep:?}");
    assert!((get_h(&m, ids[1]).unwrap() - 2.6).abs() < 1e-12, "covered row must not apply");
    assert!(rep.removed.iter().all(|r| r.id != ids[0]), "{rep:?}");
}

#[test]
fn merge_over_id_column_deletes_nothing() {
    let (mut m, _) = model();
    let p = family_book(&m, "mrg_id_del_src");
    let q = merged_book(&p, "mrg_id_del", SHEET, |_| vec![(1, 0, 2, 0)]);
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(rep.changes.iter().all(|c| c.kind() != "deleted"), "{rep:?}");
    assert!(rep.rejected.iter().any(|r| r.reason == super::import::NO_DELETES), "{rep:?}");
    // hidden rows are no deletion, so no mass deletion is asked about either
    assert!(rep.deletions.iter().all(|d| d.sheet != SHEET), "{:?}", rep.deletions);
    assert_eq!(ceilings(&m), 2);
}

#[test]
fn merged_header_is_refused_with_the_column_range() {
    let (mut m, ids) = model();
    let p = family_book(&m, "mrg_h_src");
    let q = merged_book(&p, "mrg_h", SHEET, |g| {
        let c = height_col(g);
        g[1][c] = Data::Float(3.1);
        vec![(0, c as u16, 0, c as u16 + 1)]
    });
    let rep = import_xlsx_families(&mut m, &q, opts(false), &fams()).unwrap();
    let why = rep.rejected.iter().find(|r| r.reason.starts_with("header cells are merged"));
    assert!(why.is_some_and(|r| r.blocks()), "{rep:?}");
    assert!(!rep.applied, "{rep:?}");
    assert!((get_h(&m, ids[0]).unwrap() - 2.75).abs() < 1e-12);
}

#[test]
fn merge_across_columns_fills_first_only() {
    let cell = |s: &str| Data::String(s.into());
    let mut rows = vec![
        (2, vec![Data::Float(7.0), cell("brick"), Data::Empty]),
        (3, vec![Data::Float(8.0), cell("tile"), cell("x")]),
    ];
    let head: Vec<String> = ["id", "a", "b"].map(String::from).to_vec();
    let mut rep = Report::default();
    assert_eq!(merge::apply(SHEET, 0, 0, &head, Some(0), &[], &mut rows, &mut rep), merge::Merged::default());
    // B2:C2: the value stays in B, C is not given (never a copy of B)
    let h = Dimensions { start: (1, 1), end: (1, 2) };
    let mg = merge::apply(SHEET, 0, 0, &head, Some(0), &[h], &mut rows, &mut rep);
    assert_eq!(mg.id_row, None);
    assert_eq!(rows[0].1[2], Data::Empty);
    assert_eq!(rows[1].1[2], cell("x"));
    assert!(mg.covered.contains(&(2, 2)) && !mg.covered.contains(&(3, 2)), "{mg:?}");
    let want = fill_template(merge::COVER_TPL, &["a", "2"]);
    assert!(rep.ignored.iter().any(|i| i.row == 2 && i.column == "b" && i.reason == want), "{rep:?}");
    assert!(rep.rejected.is_empty());
}

#[test]
fn block_merge_fills_down_its_first_column_only() {
    let cell = |s: &str| Data::String(s.into());
    let mut rows = vec![
        (2, vec![Data::Float(7.0), cell("brick"), cell("y")]),
        (3, vec![Data::Float(8.0), Data::Empty, Data::Empty]),
    ];
    let head: Vec<String> = ["id", "a", "b"].map(String::from).to_vec();
    let mut rep = Report::default();
    let d = Dimensions { start: (1, 1), end: (2, 2) };
    let mg = merge::apply(SHEET, 0, 0, &head, Some(0), &[d], &mut rows, &mut rep);
    assert_eq!(rows[1].1[1], cell("brick"));
    assert_eq!(rows[1].1[2], Data::Empty);
    let want: std::collections::HashSet<(u32, usize)> = [(2, 2), (3, 2)].into_iter().collect();
    assert_eq!(mg.covered, want);
}

#[test]
fn one_row_merge_over_the_id_keeps_the_row_and_reports_covered_cells() {
    let mut rows = vec![(2, vec![Data::Float(7.0), Data::Empty])];
    let head: Vec<String> = ["id", "a"].map(String::from).to_vec();
    let mut rep = Report::default();
    let d = Dimensions { start: (1, 0), end: (1, 1) };
    let mg = merge::apply(SHEET, 0, 0, &head, Some(0), &[d], &mut rows, &mut rep);
    assert_eq!(mg.id_row, None);
    assert_eq!(rows[0].1, vec![Data::Float(7.0), Data::Empty]);
    assert!(mg.covered.contains(&(2, 1)), "{mg:?}");
    let want = fill_template(merge::COVER_TPL, &["id", "2"]);
    assert!(rep.ignored.iter().any(|i| i.row == 2 && i.reason == want), "{rep:?}");
    assert!(rep.rejected.is_empty());
}

#[test]
fn one_row_merge_hiding_the_id_leaves_the_row_out() {
    let mut rows = vec![(2, vec![Data::String("note".into()), Data::Float(7.0)])];
    let head: Vec<String> = ["x", "id"].map(String::from).to_vec();
    let mut rep = Report::default();
    let d = Dimensions { start: (1, 0), end: (1, 1) };
    let mg = merge::apply(SHEET, 0, 0, &head, Some(1), &[d], &mut rows, &mut rep);
    assert_eq!(mg.id_row, Some(2));
    assert!(rows.is_empty(), "{rows:?}");
    let want = fill_template(merge::ID_TPL, &["2..2"]);
    assert!(rep.ignored.iter().any(|i| i.reason == want), "{rep:?}");
}

#[test]
fn merged_row_with_id_does_not_clear_params() {
    // the user parameter and the optional base offset sit next to a merged Mark cell
    use ddc_model::{Edit, PVal, ParamSpec, ParamType};
    let mut m = walls_model();
    let spec = ParamSpec {
        name: "Fire".into(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    };
    m.apply_edits(&[Edit::NewParam(spec)]).unwrap();
    let wid: i64 = m.sql().query_row("SELECT MIN(id) FROM wall", [], |r| r.get(0)).unwrap();
    let fire = |v: &str| Edit::ParamValue { param: "Fire".into(), kind: "wall".into(), id: wid, value: Some(PVal::Text(v.into())) };
    m.apply_edits(&[fire("EI60")]).unwrap();
    let p = tmp("mrg_param_src");
    export_xlsx(&m, &p).unwrap();
    let q = merged_book(&p, "mrg_param", WALLS, |g| {
        let (a, b) = (col(g, "id"), col(g, "Fire"));
        let r = (1..g.len()).find(|r| g[*r][a] == Data::Float(wid as f64) || g[*r][a] == Data::Int(wid)).unwrap();
        g[r][b] = Data::Empty;
        // the id merged with every column up to Fire: none of the covered cells is read
        vec![(r as u32, a as u16, r as u32, b.max(a + 1) as u16)]
    });
    let o = ImportOptions { apply: true, delete_missing: false, new_params: false };
    let rep = import_xlsx_with(&mut m, &q, o).unwrap();
    assert!(rep.changes.iter().all(|c| c.id != Some(wid)), "{:?}", rep.changes);
    assert!(rep.ignored.iter().any(|i| i.sheet == WALLS && i.reason.starts_with("merged into")), "{rep:?}");
    let d = m.param_by_name("Fire").unwrap().unwrap();
    assert_eq!(m.param_value(d.id, "wall", wid).unwrap(), Some(PVal::Text("EI60".into())));
}

#[test]
fn horizontal_merge_in_a_family_sheet_keeps_the_next_column() {
    let (mut m, ids) = model();
    let p = family_book(&m, "mrg_hz_src");
    let q = merged_book(&p, "mrg_hz", SHEET, |g| {
        let c = height_col(g);
        g[1][c] = Data::Float(3.1);
        // the id cell is merged with the cells right of it: nothing of that row is read
        vec![(2, 0, 2, c as u16)]
    });
    let rep = import_xlsx_families(&mut m, &q, opts(false), &fams()).unwrap();
    assert!(rep.rejected.is_empty(), "{rep:?}");
    assert!(rep.props.iter().all(|p| p.id != ids[1]), "covered height must not be read");
    assert!(rep.ignored.iter().any(|i| i.row == 3 && i.reason.starts_with("merged into")), "{rep:?}");
}

fn walls_model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = LayerSpec { material: "brick".into(), thickness: 0.25 };
    let t = m.add_wall_type("ext", &[layer]).unwrap();
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
    m
}

#[test]
fn category_sheet_id_merge_blocks_deletes() {
    let mut m = walls_model();
    let p = tmp("mrg_walls_src");
    export_xlsx(&m, &p).unwrap();
    let q = merged_book(&p, "mrg_walls", WALLS, |g| {
        let c = col(g, "id") as u16;
        vec![(1, c, 2, c)]
    });
    let o = ImportOptions { apply: true, delete_missing: true, new_params: false };
    let rep = import_xlsx_with(&mut m, &q, o).unwrap();
    let want = fill_template(merge::ID_TPL, &["2..3"]);
    assert!(rep.ignored.iter().any(|i| i.sheet == WALLS && i.reason == want), "{rep:?}");
    assert!(rep.rejected.iter().any(|r| r.sheet == WALLS && r.reason == super::import::NO_DELETES), "{rep:?}");
    assert_eq!(rep.counts().deleted, 0, "{:?}", rep.changes);
    assert!(rep.deletions.iter().all(|d| d.sheet != WALLS), "{:?}", rep.deletions);
    let n: i64 = m.sql().query_row("SELECT COUNT(*) FROM wall", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 3);
}
