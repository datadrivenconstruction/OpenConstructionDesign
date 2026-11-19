// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Parameter sheets of columns, beams, roofs, ceilings, slab and member types: export, edit,
//! one undo step, read-only geometry, old books without these sheets.

use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, DataType, Reader, Xlsx};
use ddc_model::{
    BeamJoin, BeamSpec, CeilingSpec, ColumnSpec, Error, Just, LayerSpec, MemberShape, Model, PVal,
    ParamSet, ParamSpec, ParamType, RoofKind, RoofSpec,
};
use rust_xlsxwriter::Workbook;

use super::*;

type Grid = Vec<Vec<Data>>;

struct Ids {
    col: i64,
    beam: i64,
    roof: i64,
    ceiling: i64,
    slab_type: i64,
    member_type: i64,
}

fn ring() -> Vec<(f64, f64)> {
    vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)]
}

fn model() -> (Model, Ids) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = LayerSpec {
        material: "concrete".into(),
        thickness: 0.2,
    };
    let st = m.add_slab_type("deck", &[layer]).unwrap();
    let mt = m
        .add_member_type("C400", &MemberShape::Rect { b: 0.4, h: 0.4 }, "concrete")
        .unwrap();
    let col = m
        .add_column(&ColumnSpec {
            level: lv,
            top_level: None,
            member_type: mt,
            structural: true,
            at: (1.0, 2.0),
            base_offset: 0.0,
            top_offset: 3.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap();
    let beam = m
        .add_beam(&BeamSpec {
            level: lv,
            member_type: mt,
            a: (0.0, 0.0, 3.0),
            b: (5.0, 0.0, 3.0),
            bulge: 0.0,
            yjust: Just::Center,
            zjust: Just::Pos,
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::Auto,
            join_end: BeamJoin::Auto,
        })
        .unwrap();
    let roof = m
        .add_roof(&RoofSpec {
            level: lv,
            slab_type: st,
            offset: 3.0,
            overhang: 0.3,
            fascia: 0.2,
            kind: RoofKind::Footprint {
                ring: ring(),
                slopes: vec![Some(30.0); 4],
                mansard: None,
            },
        })
        .unwrap();
    let ceiling = m
        .add_ceiling(&CeilingSpec {
            level: lv,
            slab_type: st,
            height: 2.7,
            outer: ring(),
            holes: vec![],
        })
        .unwrap();
    let ids = Ids {
        col,
        beam,
        roof,
        ceiling,
        slab_type: st,
        member_type: mt,
    };
    (m, ids)
}

fn spec(name: &str, t: ParamType, per_type: bool, cats: &[&str]) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype: t,
        per_type,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: cats.iter().map(|c| c.to_string()).collect(),
    }
}

fn pv(param: i64, kind: &str, id: i64, v: PVal) -> ParamSet {
    ParamSet {
        param,
        kind: kind.into(),
        id,
        value: Some(v),
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("ddc_xlsx_tests");
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{}_prm_{name}.xlsx", std::process::id()))
}

fn ops(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT COUNT(*) FROM op", [], |r| r.get(0))
        .unwrap()
}

fn export(m: &Model, name: &str) -> PathBuf {
    let p = tmp(name);
    export_xlsx(m, &p).unwrap();
    p
}

/// Rewrites a book: `keep` filters sheets, `f` edits the grids.
fn rewrite(src: &Path, dst: &Path, keep: impl Fn(&str) -> bool, f: impl Fn(&str, &mut Grid)) {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for name in wb.sheet_names() {
        if !keep(&name) {
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

fn edit(src: &Path, dst: &Path, f: impl Fn(&str, &mut Grid)) {
    rewrite(src, dst, |_| true, f)
}

fn read_sheet(p: &Path, sheet: &str) -> Grid {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.worksheet_range(sheet)
        .unwrap()
        .rows()
        .map(|r| r.to_vec())
        .collect()
}

fn col(g: &Grid, header: &str) -> usize {
    g[0].iter()
        .position(|d| d.to_string() == header)
        .unwrap_or_else(|| panic!("no column {header}: {:?}", g[0]))
}

fn row_of(g: &Grid, id: i64) -> usize {
    g.iter()
        .position(|r| r.first().and_then(|d| d.as_f64()) == Some(id as f64))
        .unwrap_or_else(|| panic!("no row {id}"))
}

fn set(g: &mut Grid, row: usize, header: &str, v: Data) {
    let c = col(g, header);
    while g[row].len() <= c {
        g[row].push(Data::Empty);
    }
    g[row][c] = v;
}

fn s(t: &str) -> Data {
    Data::String(t.into())
}

fn assert_no_op_reimport(m: &mut Model, p: &Path) {
    let n = ops(m);
    let rep = import_xlsx(m, p).unwrap();
    assert!(
        rep.applied
            && rep.changes.is_empty()
            && rep.rejected.is_empty()
            && rep.proposals.is_empty()
            && rep.ignored.is_empty()
            && rep.conflicts.is_empty(),
        "{rep:?}"
    );
    assert_eq!(ops(m), n);
}

#[test]
fn export_has_column_and_beam_params() {
    let (mut m, ids) = model();
    let mark = m.add_param(&spec("Mark", ParamType::Text, false, &["col"])).unwrap();
    let load = m.add_param(&spec("Load", ParamType::Number, false, &["beam"])).unwrap();
    let fire = m.add_param(&spec("Fire", ParamType::Text, true, &["col", "beam"])).unwrap();
    m.set_param_values(&[
        pv(mark, "col", ids.col, PVal::Text("C-1".into())),
        pv(load, "beam", ids.beam, PVal::Num(12.5)),
        pv(fire, "member_type", ids.member_type, PVal::Text("R90".into())),
    ])
    .unwrap();
    let p = export(&m, "cb");
    let g = read_sheet(&p, COLUMNS);
    let r = row_of(&g, ids.col);
    assert_eq!(g[r][col(&g, "Mark")].to_string(), "C-1");
    assert_eq!(g[r][col(&g, "level")].to_string(), "L1");
    assert_eq!(g[r][col(&g, "type")].to_string(), "C400");
    assert!((g[r][col(&g, "x [m] (read-only)")].as_f64().unwrap() - 1.0).abs() < 1e-12);
    assert!(!g[0].iter().any(|h| h.to_string() == "Load"), "beam parameter on Columns");
    let b = read_sheet(&p, BEAMS);
    let r = row_of(&b, ids.beam);
    assert!((b[r][col(&b, "Load")].as_f64().unwrap() - 12.5).abs() < 1e-12);
    let t = read_sheet(&p, MEMBER_TYPES);
    let r = row_of(&t, ids.member_type);
    assert_eq!(t[r][col(&t, "Fire")].to_string(), "R90");
    assert_eq!(t[r][col(&t, "name")].to_string(), "C400");
    assert_no_op_reimport(&mut m, &p);
}

#[test]
fn roof_param_edit_round_trips() {
    let (mut m, ids) = model();
    let cover = m.add_param(&spec("Cover", ParamType::Text, false, &["roof"])).unwrap();
    let gap = m.add_param(&spec("Gap", ParamType::Length, false, &["ceiling"])).unwrap();
    m.set_param_values(&[pv(cover, "roof", ids.roof, PVal::Text("slate".into()))])
        .unwrap();
    let p = export(&m, "roof_src");
    let q = tmp("roof_dst");
    edit(&p, &q, |sh, g| {
        if sh == ROOFS {
            let r = row_of(g, ids.roof);
            set(g, r, "Cover", s("tile"));
        }
        if sh == CEILINGS {
            let r = row_of(g, ids.ceiling);
            set(g, r, "Gap [m]", Data::Float(0.15));
        }
    });
    let n = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 2, "{:?}", rep.changes);
    assert_eq!(ops(&m), n + 1, "one Apply is one undo step");
    assert_eq!(
        m.param_value(cover, "roof", ids.roof).unwrap(),
        Some(PVal::Text("tile".into()))
    );
    let v = m.param_value(gap, "ceiling", ids.ceiling).unwrap().unwrap();
    assert!((v.as_f64().unwrap() - 0.15).abs() < 1e-12);
    m.undo().unwrap();
    assert_eq!(
        m.param_value(cover, "roof", ids.roof).unwrap(),
        Some(PVal::Text("slate".into()))
    );
    assert_eq!(m.param_value(gap, "ceiling", ids.ceiling).unwrap(), None);

    // a refused cell is reported with its address and never written
    let q = tmp("roof_bad");
    edit(&p, &q, |sh, g| {
        if sh == CEILINGS {
            let r = row_of(g, ids.ceiling);
            set(g, r, "Gap [m]", s("wide"));
        }
    });
    let n = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert_eq!(rep.rejected.len(), 1, "{rep:?}");
    assert_eq!(rep.rejected[0].sheet, CEILINGS);
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
    assert_eq!(ops(&m), n);
    assert_eq!(m.param_value(gap, "ceiling", ids.ceiling).unwrap(), None);
}

/// One column with an instance and a type parameter: the instance value is edited on its row of
/// Columns, the type value on Member types; one Apply, one undo step. A type parameter column
/// typed onto Columns is reported per column and never written.
#[test]
fn instance_and_type_param_edits_round_trip_in_one_step() {
    let (mut m, ids) = model();
    let mark = m.add_param(&spec("Mark", ParamType::Text, false, &["col"])).unwrap();
    let fire = m.add_param(&spec("Fire", ParamType::Text, true, &["col", "beam"])).unwrap();
    m.set_param_values(&[
        pv(mark, "col", ids.col, PVal::Text("C-1".into())),
        pv(fire, "member_type", ids.member_type, PVal::Text("R90".into())),
    ])
    .unwrap();
    let p = export(&m, "mix_src");
    let q = tmp("mix_dst");
    edit(&p, &q, |sh, g| {
        if sh == COLUMNS {
            let r = row_of(g, ids.col);
            set(g, r, "Mark", s("C-2"));
            // the type parameter written onto the element sheet
            g[0].push(s("Fire"));
            let c = g[0].len() - 1;
            while g[r].len() <= c {
                g[r].push(Data::Empty);
            }
            g[r][c] = s("R30");
        }
        if sh == MEMBER_TYPES {
            let r = row_of(g, ids.member_type);
            set(g, r, "Fire", s("R120"));
        }
    });
    let n = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 2, "{:?}", rep.changes);
    assert_eq!(ops(&m), n + 1, "one Apply is one undo step");
    assert_eq!(m.param_value(mark, "col", ids.col).unwrap(), Some(PVal::Text("C-2".into())));
    assert_eq!(
        m.param_value(fire, "member_type", ids.member_type).unwrap(),
        Some(PVal::Text("R120".into()))
    );
    assert_eq!(m.param_value(fire, "col", ids.col).unwrap(), None);
    let ig = rep
        .ignored
        .iter()
        .find(|i| i.sheet == COLUMNS && i.column == "Fire")
        .unwrap_or_else(|| panic!("no report entry for the type column: {:?}", rep.ignored));
    assert!(ig.reason.contains("type parameter") && ig.reason.contains("types sheet"), "{}", ig.reason);
    m.undo().unwrap();
    assert_eq!(m.param_value(mark, "col", ids.col).unwrap(), Some(PVal::Text("C-1".into())));
    assert_eq!(
        m.param_value(fire, "member_type", ids.member_type).unwrap(),
        Some(PVal::Text("R90".into()))
    );
}

#[test]
fn slab_type_param_edit_round_trips() {
    let (mut m, ids) = model();
    let code = m
        .add_param(&spec("Code", ParamType::Text, true, &["slab", "roof", "ceiling"]))
        .unwrap();
    m.set_param_values(&[pv(code, "slab_type", ids.slab_type, PVal::Text("D-1".into()))])
        .unwrap();
    let p = export(&m, "st_src");
    let g = read_sheet(&p, SLAB_TYPES);
    let r = row_of(&g, ids.slab_type);
    assert_eq!(g[r][col(&g, "Code")].to_string(), "D-1");
    assert_no_op_reimport(&mut m, &p);

    let q = tmp("st_dst");
    edit(&p, &q, |sh, g| {
        if sh == SLAB_TYPES {
            let r = row_of(g, ids.slab_type);
            set(g, r, "Code", s("D-2"));
        }
    });
    let n = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert_eq!(ops(&m), n + 1);
    assert_eq!(
        m.param_value(code, "slab_type", ids.slab_type).unwrap(),
        Some(PVal::Text("D-2".into()))
    );
    m.undo().unwrap();

    // edited in the model after the export and in the book: a conflict, never applied
    m.set_param_values(&[pv(code, "slab_type", ids.slab_type, PVal::Text("D-9".into()))])
        .unwrap();
    let n = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert_eq!(rep.conflicts.len(), 1, "{rep:?}");
    assert_eq!(rep.conflicts[0].sheet, SLAB_TYPES);
    assert_eq!(ops(&m), n);
    assert_eq!(
        m.param_value(code, "slab_type", ids.slab_type).unwrap(),
        Some(PVal::Text("D-9".into()))
    );
}

#[test]
fn old_book_without_new_sheets_imports() {
    let (mut m, ids) = model();
    let cover = m.add_param(&spec("Cover", ParamType::Text, false, &["roof"])).unwrap();
    m.set_param_values(&[pv(cover, "roof", ids.roof, PVal::Text("slate".into()))])
        .unwrap();
    let p = export(&m, "old_src");
    let new_sheets = [COLUMNS, BEAMS, ROOFS, CEILINGS, SLAB_TYPES, MEMBER_TYPES];
    let q = tmp("old_dst");
    // an old book: no parameter sheets, and its baseline never listed them
    rewrite(
        &p,
        &q,
        |sh| !new_sheets.contains(&sh),
        |sh, g| {
            if sh == BASELINE {
                let mut drop = false;
                g.retain(|r| {
                    if r.first().map(|d| d.to_string()) == Some("#".into()) {
                        let sheet = r.get(1).map(|d| d.to_string()).unwrap_or_default();
                        drop = new_sheets.contains(&sheet.as_str());
                    }
                    !drop
                });
            }
        },
    );
    assert!(!sheet_names(&q).unwrap().iter().any(|n| n == COLUMNS));
    let n = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(
        rep.applied
            && rep.changes.is_empty()
            && rep.rejected.is_empty()
            && rep.ignored.is_empty()
            && rep.sheet_issues.is_empty(),
        "{rep:?}"
    );
    assert_eq!(ops(&m), n);
    assert_eq!(
        m.param_value(cover, "roof", ids.roof).unwrap(),
        Some(PVal::Text("slate".into())),
        "a sheet missing from an old book changes nothing"
    );
}

#[test]
fn read_only_geometry_edit_refused_or_ignored() {
    let (mut m, ids) = model();
    let mark = m.add_param(&spec("Mark", ParamType::Text, false, &["col"])).unwrap();
    let p = export(&m, "ro_src");
    let q = tmp("ro_dst");
    edit(&p, &q, |sh, g| {
        if sh == COLUMNS {
            let r = row_of(g, ids.col);
            set(g, r, "x [m] (read-only)", Data::Float(99.0));
            set(g, r, "level", s("L9"));
            set(g, r, "Mark", s("C-7"));
        }
    });
    let x = |m: &Model| -> f64 {
        m.sql()
            .query_row("SELECT x FROM col WHERE id=?1", [ids.col], |r| r.get(0))
            .unwrap()
    };
    let n = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    let ro: Vec<&Ignored> = rep.ignored.iter().filter(|i| i.sheet == COLUMNS).collect();
    assert_eq!(ro.len(), 2, "{:?}", rep.ignored);
    assert!(ro.iter().all(|i| i.reason.starts_with("read-only column") && i.id == Some(ids.col)));
    assert!(ro.iter().any(|i| i.column == "x [m] (read-only)" && i.new == "99"), "{ro:?}");
    assert_eq!(x(&m), 1.0, "geometry is never written from the book");
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert_eq!(ops(&m), n + 1);
    assert_eq!(
        m.param_value(mark, "col", ids.col).unwrap(),
        Some(PVal::Text("C-7".into()))
    );
}

fn get_x(m: &Model, id: i64) -> Result<f64, Error> {
    Ok(m.sql()
        .query_row("SELECT x FROM col WHERE id=?1", [id], |r| r.get(0))?)
}

fn set_none(_: &mut Model, _: i64, _: f64) -> Result<(), Error> {
    Ok(())
}

fn col_family() -> Vec<FamilySheet> {
    vec![FamilySheet {
        sheet: COLUMNS,
        table: "col",
        cols: vec![FamilyCol {
            key: "x",
            label: "X".into(),
            dim: FamDim::Len,
            labels: vec![],
            lo: -1e6,
            hi: 1e6,
            get: get_x,
            set: set_none,
        }],
        delete: RowDelete::Allowed,
    }]
}

#[test]
fn family_sheet_carries_params_and_edits_apply() {
    let (mut m, ids) = model();
    let mark = m.add_param(&spec("Mark", ParamType::Text, false, &["col"])).unwrap();
    m.set_param_values(&[pv(mark, "col", ids.col, PVal::Text("C-1".into()))])
        .unwrap();
    let p = tmp("fam_src");
    export_xlsx_families(&m, &p, Units::Metric, &col_family()).unwrap();
    let g = read_sheet(&p, COLUMNS);
    let r = row_of(&g, ids.col);
    assert_eq!(g[r][col(&g, "Mark")].to_string(), "C-1");
    assert!(g[0].iter().any(|h| h.to_string() == "X [m]"), "family layout: {:?}", g[0]);
    let opts = ImportOptions {
        apply: true,
        delete_missing: false,
        new_params: false,
    };
    let rep = import_xlsx_families(&mut m, &p, opts, &col_family()).unwrap();
    assert!(
        rep.applied && rep.changes.is_empty() && rep.ignored.is_empty() && rep.proposals.is_empty(),
        "{rep:?}"
    );
    let q = tmp("fam_dst");
    edit(&p, &q, |sh, g| {
        if sh == COLUMNS {
            let r = row_of(g, ids.col);
            set(g, r, "Mark", s("C-2"));
        }
    });
    let rep = import_xlsx_families(&mut m, &q, opts, &col_family()).unwrap();
    assert!(rep.applied && rep.rejected.is_empty() && rep.ignored.is_empty(), "{rep:?}");
    assert_eq!(
        m.param_value(mark, "col", ids.col).unwrap(),
        Some(PVal::Text("C-2".into()))
    );
}

#[test]
fn styled_param_sheet_inserted_column_maps_by_caption() {
    let (mut m, ids) = model();
    let mark = m.add_param(&spec("Mark", ParamType::Text, false, &["col"])).unwrap();
    m.set_param_values(&[pv(mark, "col", ids.col, PVal::Text("C-1".into()))])
        .unwrap();
    let caps: std::collections::HashMap<String, String> =
        [("x", "Pos X"), ("y", "Pos Y"), ("level", "Storey"), ("type", "Kind")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into();
    let p = tmp("styled_ins_src");
    export_xlsx_styled(&m, &p, &p, Units::Metric, &[], &Style::professional(caps)).unwrap();
    let keys = read_sheet(&p, KEYS);
    let row = format!("{COLUMNS}:captions");
    assert!(keys.iter().any(|r| r.first().is_some_and(|d| d.to_string() == row)), "{keys:?}");
    let q = tmp("styled_ins_dst");
    edit(&p, &q, |sh, g| {
        if sh == COLUMNS {
            // a number column before x: read by position it would edit the read-only x
            let c = col(g, "Pos X [m]");
            for (i, r) in g.iter_mut().enumerate() {
                while r.len() < c {
                    r.push(Data::Empty);
                }
                r.insert(c, if i == 0 { s("Notes") } else { Data::Float(9.0) });
            }
            let r = row_of(g, ids.col);
            set(g, r, "Mark", s("C-2"));
        }
    });
    let n = ops(&m);
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert_eq!(ops(&m), n + 1);
    assert_eq!(
        m.param_value(mark, "col", ids.col).unwrap(),
        Some(PVal::Text("C-2".into()))
    );
    let x: f64 = m
        .sql()
        .query_row("SELECT x FROM col WHERE id=?1", [ids.col], |r| r.get(0))
        .unwrap();
    assert!((x - 1.0).abs() < 1e-12, "the inserted column was read as x");
}

/// A roof with a Text parameter `Cover` holding `code`, and its exported book.
fn cover_book(code: &str, name: &str) -> (Model, Ids, i64, PathBuf) {
    let (mut m, ids) = model();
    let cover = m.add_param(&spec("Cover", ParamType::Text, false, &["roof"])).unwrap();
    m.set_param_values(&[pv(cover, "roof", ids.roof, PVal::Text(code.into()))])
        .unwrap();
    let p = export(&m, name);
    (m, ids, cover, p)
}

fn cover_of(m: &Model, cover: i64, ids: &Ids) -> Option<PVal> {
    m.param_value(cover, "roof", ids.roof).unwrap()
}

#[test]
fn text_code_turned_into_number_keeps_model_text() {
    let cases = [
        ("007", 7.0),
        ("1.10", 1.1),
        ("1E3", 1000.0),
        ("00123", 123.0),
        ("1,000", 1000.0),
        ("1234567890123456789", 1.23456789012346e18),
    ];
    for (i, (code, num)) in cases.into_iter().enumerate() {
        let (mut m, ids, cover, p) = cover_book(code, &format!("tc_src{i}"));
        let q = tmp(&format!("tc_dst{i}"));
        edit(&p, &q, |sh, g| {
            if sh == ROOFS {
                let r = row_of(g, ids.roof);
                set(g, r, "Cover", Data::Float(num));
            }
        });
        let n = ops(&m);
        let rep = import_xlsx(&mut m, &q).unwrap();
        // a code past 15 digits is refused; a shorter one is kept with a note
        let lossy = code.len() > 15;
        let (row, col, sheet, column, reason) = if lossy {
            assert!(rep.ignored.is_empty(), "{code}: {rep:?}");
            assert_eq!(rep.rejected.len(), 1, "{code}: {rep:?}");
            let x = &rep.rejected[0];
            (x.row, x.col, x.sheet.clone(), x.column.clone(), x.reason.clone())
        } else {
            assert!(rep.rejected.is_empty(), "{code}: {rep:?}");
            assert_eq!(rep.ignored.len(), 1, "{code}: {rep:?}");
            let x = &rep.ignored[0];
            (x.row, x.col, x.sheet.clone(), x.column.clone(), x.reason.clone())
        };
        assert_eq!((sheet.as_str(), column.as_str()), (ROOFS, "Cover"), "{code}");
        assert!(row > 1 && col > 0, "{code}: the cell is named");
        assert!(reason.contains("turned this code into a number"), "{code}: {reason}");
        assert_eq!(reason.contains("dropped digits"), lossy, "{code}: {reason}");
        assert!(rep.changes.is_empty() && rep.model_newer == 0, "{code}: {:?}", rep.changes);
        assert_eq!(ops(&m), n, "{code}");
        assert_eq!(cover_of(&m, cover, &ids), Some(PVal::Text(code.into())), "{code}");
    }
}

#[test]
fn text_param_genuine_numeric_edit_still_applies() {
    let (mut m, ids, cover, p) = cover_book("7", "tc_gen_src");
    let q = tmp("tc_gen_dst");
    edit(&p, &q, |sh, g| {
        if sh == ROOFS {
            let r = row_of(g, ids.roof);
            set(g, r, "Cover", Data::Float(8.0));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert_eq!(cover_of(&m, cover, &ids), Some(PVal::Text("8".into())));
}

#[test]
fn text_param_equal_number_reading_is_no_change() {
    let (mut m, ids, cover, p) = cover_book("12", "tc_eq_src");
    let q = tmp("tc_eq_dst");
    edit(&p, &q, |sh, g| {
        if sh == ROOFS {
            let r = row_of(g, ids.roof);
            set(g, r, "Cover", Data::Float(12.0));
        }
    });
    assert_no_op_reimport(&mut m, &q);
    assert_eq!(cover_of(&m, cover, &ids), Some(PVal::Text("12".into())));
}

#[test]
fn numeric_param_is_not_affected() {
    let (mut m, ids) = model();
    let gap = m.add_param(&spec("Gap", ParamType::Number, false, &["roof"])).unwrap();
    m.set_param_values(&[pv(gap, "roof", ids.roof, PVal::Num(7.0))]).unwrap();
    let p = export(&m, "tc_num_src");
    let q = tmp("tc_num_dst");
    edit(&p, &q, |sh, g| {
        if sh == ROOFS {
            let r = row_of(g, ids.roof);
            set(g, r, "Gap", Data::Float(7.5));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    let v = m.param_value(gap, "roof", ids.roof).unwrap().unwrap();
    assert!((v.as_f64().unwrap() - 7.5).abs() < 1e-12);
}

#[test]
fn export_writes_text_param_as_text_cell_and_round_trips() {
    let (mut m, ids, cover, p) = cover_book("007", "tc_rt_src");
    let g = read_sheet(&p, ROOFS);
    let (r, c) = (row_of(&g, ids.roof), col(&g, "Cover"));
    assert_eq!(g[r][c], Data::String("007".into()));
    let mut z = zip::ZipArchive::new(std::fs::File::open(&p).unwrap()).unwrap();
    let mut styles = String::new();
    std::io::Read::read_to_string(&mut z.by_name("xl/styles.xml").unwrap(), &mut styles).unwrap();
    assert!(styles.contains("numFmtId=\"49\""), "Text number format missing");
    assert_no_op_reimport(&mut m, &p);
    assert_eq!(cover_of(&m, cover, &ids), Some(PVal::Text("007".into())));
}

/// Imports a Columns book where the read-only `x` of the column is `xm` in the model and
/// `cell` in the book (header renamed to `head` when given); returns the ignored rows of the sheet.
fn ro_cell(xm: f64, head: Option<&str>, cell: Data, comma: bool, tag: &str) -> Vec<Ignored> {
    let (mut m, ids) = model();
    m.sql()
        .execute("UPDATE col SET x=?1 WHERE id=?2", [xm, ids.col as f64])
        .unwrap();
    let st = Style::default().with_number_format(comma, if comma { "de-DE" } else { "en-US" });
    let p = tmp(&format!("{tag}_src"));
    export_xlsx_styled(&m, &p, &p, Units::Metric, &[], &st).unwrap();
    let q = tmp(&format!("{tag}_dst"));
    edit(&p, &q, |sh, g| {
        if sh == COLUMNS {
            let r = row_of(g, ids.col);
            let c = col(g, "x [m] (read-only)");
            if let Some(h) = head {
                g[0][c] = s(h);
            }
            while g[r].len() <= c {
                g[r].push(Data::Empty);
            }
            g[r][c] = cell.clone();
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    rep.ignored.into_iter().filter(|i| i.sheet == COLUMNS && i.column.starts_with("x [")).collect()
}

#[test]
fn read_only_formatted_text_equal_to_model_is_not_flagged() {
    let nbsp = "1\u{a0}250,5";
    assert!(ro_cell(1250.5, None, s(nbsp), false, "ro_nbsp").is_empty());
    assert!(ro_cell(1250.5, None, s("1 250,5"), false, "ro_sp").is_empty());
    assert!(ro_cell(1250.5, None, s("1,250.5"), false, "ro_en").is_empty());
    assert!(ro_cell(1250.5, None, s("1.250,5"), false, "ro_ru").is_empty());
    assert!(ro_cell(1250.5, None, s(nbsp), true, "ro_nbsp_c").is_empty());
    assert!(ro_cell(1250.5, None, Data::Float(1250.5), false, "ro_num").is_empty());
}

#[test]
fn read_only_length_text_with_unit_equals_model() {
    assert!(ro_cell(3.5, None, s("3500 mm"), false, "ro_mm_txt").is_empty());
    assert!(ro_cell(3.5, None, s("3.5 m"), false, "ro_m_txt").is_empty());
    assert!(ro_cell(3.5, Some("x [mm] (read-only)"), Data::Float(3500.0), false, "ro_mm_col").is_empty());
    assert!(ro_cell(3.5, Some("x [mm] (read-only)"), s("3 500"), false, "ro_mm_grp").is_empty());
}

#[test]
fn read_only_ambiguous_group_matches_either_reading() {
    assert!(ro_cell(1250.0, None, s("1,250"), false, "ro_amb_a").is_empty());
    assert!(ro_cell(1.25, None, s("1,250"), true, "ro_amb_b").is_empty());
    let bad = ro_cell(7.0, None, s("1,250"), false, "ro_amb_c");
    assert_eq!(bad.len(), 1, "{bad:?}");
}

#[test]
fn read_only_real_change_is_flagged_with_clean_value() {
    let bad = ro_cell(3.5, Some("x [mm] (read-only)"), Data::Float(4000.0), false, "ro_chg");
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert_eq!(bad[0].old, "3500");
    assert_eq!(bad[0].reason, "read-only column, computed value is 3500; edit not accepted");
    let bad = ro_cell(3.5, None, s("4000 mm"), false, "ro_chg_txt");
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert_eq!(bad[0].old, "3.5");
    // float noise never reaches the message
    let bad = ro_cell(0.1 + 0.2, None, Data::Float(9.0), false, "ro_noise");
    assert_eq!(bad[0].old, "0.3", "{bad:?}");
}

#[test]
fn read_only_message_uses_book_decimal_mark() {
    let bad = ro_cell(1250.5, None, Data::Float(1.0), true, "ro_msg_c");
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert_eq!(bad[0].old, "1250,5");
    assert_eq!(bad[0].reason, "read-only column, computed value is 1250,5; edit not accepted");
    let bad = ro_cell(1250.5, None, Data::Float(1.0), false, "ro_msg_p");
    assert_eq!(bad[0].old, "1250.5");
}

#[test]
fn read_only_negative_zero_and_empty() {
    // comma text in a comma book; `2,5` in a point book awaits a decimal rule
    assert!(ro_cell(-2.5, None, s("\u{2212}2,5"), true, "ro_neg").is_empty());
    assert!(ro_cell(-2.5, None, s("-2500 mm"), false, "ro_neg_mm").is_empty());
    assert!(ro_cell(0.0, None, s("0,0"), true, "ro_zero").is_empty());
    let bad = ro_cell(-2.5, None, Data::Float(2.5), false, "ro_neg_sign");
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert_eq!(bad[0].old, "-2.5");
    // a cleared cell is a refused edit, not a silent match
    let bad = ro_cell(3.5, None, Data::Empty, false, "ro_empty");
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert_eq!(bad[0].new, "");
}

#[test]
fn ro_shown_trims_noise_and_zeros() {
    use super::import::ro_shown;
    assert_eq!(ro_shown(3500.0000000000005, "x [mm]"), "3500");
    assert_eq!(ro_shown(2.0, "area [m2]"), "2");
    assert_eq!(ro_shown(-0.0, "x [m]"), "0");
    assert_eq!(ro_shown(0.0004, "x [m]"), "0.0004");
}
