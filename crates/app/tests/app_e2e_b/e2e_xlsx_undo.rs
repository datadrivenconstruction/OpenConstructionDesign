// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Undo of an XLSX Apply is total: every model table before the Apply equals the model after
//! Undo and after Redo + Undo (created and deleted rows, family type changes, schedule book rows,
//! locked parameters, pending links), and the record of the rows the Apply created in the book
//! goes and comes back with the step, so the next sync neither offers a row twice nor misses one.
//! The Apply is driven as the XLSX window does it: preview, then `apply_xlsx_linked`.

use crate::e2e_common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_app::editor::{Editor, XlsxLink, OpFn};
use ddc_app::elem::sheets::families;
use ddc_app::shell::xlsx_links;
use ddc_app::shell::xlsx_win::XlsxPreview;
use ddc_io::xlsx::{self, FamDim, TableCell as C, Units};
use ddc_model::{
    ColumnSpec, Edit, Model, OpeningSpec, PVal, ParamSpec, ParamType, RoomSpec, WallSpec,
};
use e2e_common::{assert_same, dump_except, ids, tables, temp_file, TableRows};
use rust_xlsxwriter::Workbook;

type Grid = Vec<Vec<Data>>;

const MARK: &str = "Mark";
const SRC: &str = "ifc:xs3";

/// Tables recomputed from the model after every step (and after undo), with fresh row ids, so
/// their rows are not the user's data: quantities (`*_qty`), cut bodies (`*_cut`), caches
/// (`join_cache`, `mass_cache`), structural checks, solid bases, wall joins, dimension geometry.
fn derived(t: &str) -> bool {
    t.ends_with("_qty")
        || t.ends_with("_cut")
        || t.ends_with("_cache")
        || ["struct_issue", "solid_base", "geom_join", "dim_geo"].contains(&t)
}

/// Every table but the derived ones; `journal` keeps the undo journal (`op`) in.
fn state(m: &Model, journal: bool) -> Vec<TableRows> {
    let mut skip: Vec<String> = tables(m).into_iter().filter(|t| derived(t)).collect();
    if !journal {
        skip.push("op".into());
    }
    let skip: Vec<&str> = skip.iter().map(String::as_str).collect();
    dump_except(m, &skip)
}

fn int(m: &Model, sql: &str) -> i64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

struct Fx {
    ed: Editor,
    /// Walls: `a` has a locked Mark, `b` a door, `d` a pending link.
    a: i64,
    c: i64,
    d: i64,
    cols: Vec<i64>,
    mtypes: [i64; 2],
}

/// 6x4 box of four walls with a door and a room, Mark on every wall (locked on `a`), two columns
/// of two member types, and a pending material link of an IFC source on wall `d`.
fn scene() -> Fx {
    let mut ed = Editor::new_in_memory().unwrap();
    let l = ed.level;
    let wt = ids(&ed.model, "wall_type")[0];
    let m = &mut ed.model;
    let corners = [
        ((0.0, 0.0), (6.0, 0.0)),
        ((6.0, 0.0), (6.0, 4.0)),
        ((6.0, 4.0), (0.0, 4.0)),
        ((0.0, 4.0), (0.0, 0.0)),
    ];
    let w: Vec<i64> = corners
        .iter()
        .map(|&(a, b)| {
            m.add_wall(&WallSpec { a, b, level: l, base_offset: 0.0, height: 3.0, wall_type: wt, offset0: None })
                .unwrap()
        })
        .collect();
    m.add_opening(w[1], &OpeningSpec { along: 1.0, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() })
        .unwrap();
    m.add_room(&RoomSpec { level: l, name: "Kitchen".into(), number: "101".into(), point: (3.0, 2.0), height: 2.7 })
        .unwrap();
    m.add_params(&[ParamSpec {
        name: MARK.into(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    }])
    .unwrap();
    for (i, id) in w.iter().enumerate() {
        m.apply_edits(&[Edit::ParamValue {
            param: MARK.into(),
            kind: "wall".into(),
            id: *id,
            value: Some(PVal::Text(format!("W{i}"))),
        }])
        .unwrap();
    }
    m.set_param_lock(&[("wall", w[0], MARK)]).unwrap();
    let mtypes = [m.add_catalog_member_type("R 400x400").unwrap(), m.add_catalog_member_type("IPE 200").unwrap()];
    let cols: Vec<i64> = (0..2)
        .map(|i| {
            m.add_column(&ColumnSpec {
                level: l,
                top_level: None,
                member_type: mtypes[i],
                structural: true,
                at: (20.0 + 4.0 * i as f64, 0.0),
                base_offset: 0.0,
                top_offset: 3.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap()
        })
        .collect();
    // a link an earlier save could not write: the base row of the source and its element
    m.sql()
        .execute(
            "INSERT INTO src_base(source,guid,kind,vals) VALUES (?1,'G-d','wall',?2)",
            rusqlite::params![SRC, r#"{"~link":["material"]}"#],
        )
        .unwrap();
    m.sql()
        .execute("INSERT INTO ext_ref(source,guid,kind,ref) VALUES (?1,'G-d','wall',?2)", rusqlite::params![SRC, w[3]])
        .unwrap();
    assert_eq!(m.pending_links(SRC).unwrap().len(), 1, "fixture: one pending link");
    ed.reload().unwrap();
    Fx { ed, a: w[0], c: w[2], d: w[3], cols, mtypes }
}

// ---------------------------------------------------------------- books

fn row_id(d: &Data) -> Option<i64> {
    match d {
        Data::Float(x) => Some(*x as i64),
        Data::Int(i) => Some(*i),
        _ => None,
    }
}

/// Column of `header`, appended to the header row when the sheet lacks it.
fn col(g: &mut Grid, header: &str) -> usize {
    if let Some(c) = g[0].iter().position(|d| d.to_string() == header) {
        return c;
    }
    g[0].push(Data::String(header.into()));
    g[0].len() - 1
}

fn row_of(g: &Grid, id: i64) -> usize {
    (1..g.len())
        .find(|&r| g[r].first().and_then(row_id) == Some(id))
        .unwrap_or_else(|| panic!("no row with id {id}"))
}

fn put(g: &mut Grid, r: usize, c: usize, v: Data) {
    while g[r].len() <= c {
        g[r].push(Data::Empty);
    }
    g[r][c] = v;
}

fn set_by_id(g: &mut Grid, id: i64, header: &str, v: Data) {
    let c = col(g, header);
    let r = row_of(g, id);
    put(g, r, c, v);
}

/// A new id-less wall row: the geometry columns of the row of `like`, moved `dx` along x.
fn add_wall_row(g: &mut Grid, like: i64, dx: f64) {
    const KEEP: [&str; 8] =
        ["level", "type", "ax [m]", "ay [m]", "bx [m]", "by [m]", "base_offset [m]", "height [m]"];
    let src = g[row_of(g, like)].clone();
    let heads: Vec<String> = g[0].iter().map(|d| d.to_string()).collect();
    let mut row: Vec<Data> = heads
        .iter()
        .enumerate()
        .map(|(i, h)| if KEEP.contains(&h.as_str()) { src.get(i).cloned().unwrap_or(Data::Empty) } else { Data::Empty })
        .collect();
    for h in ["ax [m]", "bx [m]"] {
        let i = heads.iter().position(|x| x == h).unwrap_or_else(|| panic!("no column {h}"));
        let v = match &row[i] {
            Data::Float(x) => *x,
            Data::Int(x) => *x as f64,
            other => panic!("{h} is {other:?}"),
        };
        row[i] = Data::Float(v + dx);
    }
    g.push(row);
}

/// The book rewritten with every sheet's grid passed through `f`.
fn edit_book(src: &Path, dst: &Path, f: impl Fn(&str, &mut Grid)) {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for name in wb.sheet_names() {
        let mut grid: Grid = wb.worksheet_range(&name).unwrap().rows().map(|r| r.to_vec()).collect();
        f(&name, &mut grid);
        let ws = out.add_worksheet();
        ws.set_name(&name).unwrap();
        for (i, row) in grid.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(drop),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(drop),
                    Data::String(s) => ws.write_string(i, j, s).map(drop),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(dst).unwrap();
}

/// The whole model exported with the family sheets, then edited by `f`.
fn book(fx: &Fx, tag: &str, f: impl Fn(&str, &mut Grid)) -> PathBuf {
    let src = temp_file(&format!("xs3_{tag}_src.xlsx"));
    xlsx::export_xlsx_families(&fx.ed.model, &src, Units::Metric, &families()).unwrap();
    let dst = temp_file(&format!("xs3_{tag}.xlsx"));
    edit_book(&src, &dst, f);
    dst
}

/// Header of the member type column of the Columns sheet.
fn column_type_head() -> String {
    let fams = families();
    let sheet = fams.iter().find(|s| s.sheet == "Columns").expect("Columns family sheet");
    let c = sheet.cols.iter().find(|c| matches!(c.dim, FamDim::Type(_))).expect("Columns type column");
    c.header(Units::Metric)
}

// ---------------------------------------------------------------- the Apply, as the window does it

/// The book read against the model (a sync); the model is not touched.
fn sync(ed: &mut Editor, book: &Path) -> XlsxPreview {
    XlsxPreview::load(&mut ed.model, book, false).unwrap()
}

/// The first read of a book: every cell is valid.
fn preview(ed: &mut Editor, book: &Path) -> XlsxPreview {
    let pv = sync(ed, book);
    assert!(pv.report.rejected.is_empty(), "{:?}", pv.report.rejected);
    pv
}

/// `apply_begin` without the re-export: the rows it creates are remembered for the book.
fn apply(ed: &mut Editor, pv: &XlsxPreview) {
    let rep = &pv.report;
    let rows: Vec<(String, u32, String)> = rep
        .changes
        .iter()
        .filter(|c| c.kind() == "added")
        .map(|c| {
            let fp = rep.fingerprints.get(&(c.sheet.clone(), c.row)).cloned().unwrap_or_default();
            (c.sheet.clone(), c.row, fp)
        })
        .collect();
    let link = (!rows.is_empty()).then(|| Arc::new(XlsxLink { book: pv.path.clone(), rows }));
    ed.apply_xlsx_linked(rep.batch(), rep.props.clone(), "xs3", link).unwrap();
}

fn made(ed: &Editor, book: &Path) -> usize {
    xlsx_links::load(&ed.model, book).values().map(Vec::len).sum()
}

/// Apply, Undo, Redo, Undo of `book`, with the totality and next-sync checks at each point.
fn cycle(ed: &mut Editor, book: &Path) {
    let s0 = state(&ed.model, true);
    let pv = preview(ed, book);
    let want = pv.report.counts();
    assert!(want.added > 0, "the book creates rows: {:?}", pv.report.changes);
    assert_same(&state(&ed.model, true), &s0, "the preview writes nothing");

    apply(ed, &pv);
    let s1 = state(&ed.model, false);
    assert_ne!(s1, state_data(&s0), "the Apply changed nothing");
    assert_eq!(made(ed, book), want.added, "every created row is remembered");
    assert_eq!(sync(ed, book).report.counts().added, 0, "after Apply: created rows are not offered again");

    ed.undo();
    assert_same(&state(&ed.model, true), &s0, "undo of the Apply");
    assert_eq!(made(ed, book), 0, "undo forgets the created rows");
    assert_eq!(sync(ed, book).report.counts(), want, "after undo: the next sync offers what the first did");
    assert_same(&state(&ed.model, true), &s0, "a sync after undo writes nothing");

    ed.redo();
    assert_same(&state(&ed.model, false), &s1, "redo of the Apply");
    assert_eq!(made(ed, book), want.added, "redo remembers the created rows again");
    assert_eq!(sync(ed, book).report.counts().added, 0, "after redo: created rows are not offered again");

    ed.undo();
    assert_same(&state(&ed.model, true), &s0, "redo + undo of the Apply");
    assert_eq!(made(ed, book), 0);
}

/// `s` without the journal, as `state(m, false)` would read it.
fn state_data(s: &[TableRows]) -> Vec<TableRows> {
    s.iter().filter(|t| t.0 != "op").cloned().collect()
}

// ---------------------------------------------------------------- tests

/// Cells, a new row, a deleted row with a pending link, a family type change and a locked Mark,
/// all in one book: one Apply, undone whole.
#[test]
fn xlsx_undo_restores_every_table() {
    let mut fx = scene();
    let (a, c, d, col0, other) = (fx.a, fx.c, fx.d, fx.cols[0], fx.mtypes[1]);
    let type_head = column_type_head();
    let p = book(&fx, "all", |sheet, g| match sheet {
        "Walls" => {
            set_by_id(g, a, "height [m]", Data::Float(3.4));
            set_by_id(g, a, MARK, Data::String("X".into()));
            set_by_id(g, c, "height [m]", Data::Float(3.2));
            set_by_id(g, d, "delete", Data::String("x".into()));
            add_wall_row(g, a, 40.0);
        }
        "Columns" => set_by_id(g, col0, &type_head, Data::Float(other as f64)),
        _ => {}
    });
    let pv = preview(&mut fx.ed, &p);
    let n = pv.report.counts();
    assert_eq!((n.added, n.deleted), (1, 1), "{:?}", pv.report.changes);
    assert!(!pv.report.props.is_empty(), "the family type change is a family sheet edit");
    assert!(
        pv.report.ignored.iter().any(|i| i.column == MARK && i.reason.contains("locked")),
        "{:?}",
        pv.report.ignored
    );

    cycle(&mut fx.ed, &p);
    let m = &fx.ed.model;
    assert_eq!(int(m, &format!("SELECT count(*) FROM wall WHERE id={d}")), 1, "the deleted wall is back");
    assert_eq!(m.pending_links(SRC).unwrap().len(), 1, "its pending link is back");
    assert!(m.param_locked("wall", a, MARK).unwrap(), "the lock stays");
    let mark = m.param_by_name(MARK).unwrap().unwrap();
    assert_eq!(m.param_value(mark.id, "wall", a).unwrap(), Some(PVal::Text("W0".into())), "the locked value");
    assert_eq!(int(m, &format!("SELECT mtype FROM col WHERE id={col0}")), fx.mtypes[0]);
}

/// A family sheet edit makes the Apply several model ops merged into one journal entry: the
/// created row is still remembered after the merge.
#[test]
fn xlsx_family_edit_keeps_created_row_known() {
    let mut fx = scene();
    let (a, col0, other) = (fx.a, fx.cols[0], fx.mtypes[1]);
    let type_head = column_type_head();
    let p = book(&fx, "fam", |sheet, g| match sheet {
        "Walls" => add_wall_row(g, a, 40.0),
        "Columns" => set_by_id(g, col0, &type_head, Data::Float(other as f64)),
        _ => {}
    });
    let pv = preview(&mut fx.ed, &p);
    assert_eq!(pv.report.counts().added, 1, "{:?}", pv.report.changes);
    assert!(!pv.report.props.is_empty() && !pv.report.batch().is_empty());
    let walls = ids(&fx.ed.model, "wall").len();
    apply(&mut fx.ed, &pv);
    assert_eq!(ids(&fx.ed.model, "wall").len(), walls + 1);
    assert_eq!(made(&fx.ed, &p), 1, "the created row is remembered");
    let again = sync(&mut fx.ed, &p);
    assert_eq!(again.report.counts().added, 0, "the next sync does not offer the row again");
    apply(&mut fx.ed, &again);
    assert_eq!(ids(&fx.ed.model, "wall").len(), walls + 1, "nothing is created twice");
}

fn walls_rows(m: &Model) -> Vec<Vec<C>> {
    let mut st = m
        .sql()
        .prepare(
            "SELECT w.id,lv.name,t.name,w.ax,w.ay,w.bx,w.by,w.base_offset,w.height FROM wall w \
             JOIN level lv ON lv.id=w.level JOIN wall_type t ON t.id=w.wtype ORDER BY w.id",
        )
        .unwrap();
    st.query_map([], |r| {
        let mut v = vec![C::Num(r.get::<_, i64>(0)? as f64), C::Text(r.get(1)?), C::Text(r.get(2)?)];
        for i in 3..9 {
            v.push(C::Num(r.get(i)?));
        }
        Ok(v)
    })
    .unwrap()
    .map(|x| x.unwrap())
    .collect()
}

/// A schedule book (one sheet, totals row, no baseline): an edited height and a new row.
#[test]
fn xlsx_undo_of_schedule_book() {
    let mut fx = scene();
    let p = temp_file("xs3_schedule.xlsx");
    let mut rows = walls_rows(&fx.ed.model);
    rows[0][8] = C::Num(3.3);
    let mut new = rows[0].clone();
    new[0] = C::Text(String::new());
    new[3] = C::Num(40.0);
    new[5] = C::Num(46.0);
    rows.push(new);
    let heads: Vec<String> =
        ["id", "level", "type", "ax [m]", "ay [m]", "bx [m]", "by [m]", "base_offset [m]", "height [m]"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    let mut tot = vec![C::CountLabel { pre: "Total: ".into(), post: String::new(), n: rows.len() }];
    tot.extend((1..9).map(|j| if j < 3 { C::Text(String::new()) } else { C::Subtotal(109, 1.0) }));
    xlsx::export_table(&p, "Walls", &heads, &[], &rows, Some(&tot)).unwrap();
    let pv = preview(&mut fx.ed, &p);
    let n = pv.report.counts();
    assert_eq!((n.added, n.changed, n.deleted), (1, 1, 0), "{:?}", pv.report.changes);
    cycle(&mut fx.ed, &p);
}

/// The book adds one wall row; returns the book and the wall its Apply created.
fn applied_new_wall(fx: &mut Fx, tag: &str) -> (PathBuf, i64) {
    let a = fx.a;
    let p = book(fx, tag, |sheet, g| {
        if sheet == "Walls" {
            add_wall_row(g, a, 40.0)
        }
    });
    let pv = preview(&mut fx.ed, &p);
    assert_eq!(pv.report.counts().added, 1, "{:?}", pv.report.changes);
    let before = ids(&fx.ed.model, "wall");
    apply(&mut fx.ed, &pv);
    let new: Vec<i64> = ids(&fx.ed.model, "wall").into_iter().filter(|i| !before.contains(i)).collect();
    assert_eq!(new.len(), 1, "{new:?}");
    assert_eq!(made(&fx.ed, &p), 1);
    (p, new[0])
}

fn delete_wall(ed: &mut Editor, id: i64) {
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.delete_walls(&[id])))).unwrap();
}

/// Apply #1 creates E, a later step deletes E, a sync runs, the delete is undone: E and the
/// record of its row are back together and the next sync offers nothing (no duplicate).
#[test]
fn xlsx_undo_of_delete_keeps_created_row_known() {
    let mut fx = scene();
    let (p, e) = applied_new_wall(&mut fx, "del");
    let s0 = state(&fx.ed.model, true);
    delete_wall(&mut fx.ed, e);
    assert_eq!(sync(&mut fx.ed, &p).report.counts().added, 1, "while E is gone its row is offered");
    assert_eq!(made(&fx.ed, &p), 0);
    let s1 = state(&fx.ed.model, true);
    assert_eq!(sync(&mut fx.ed, &p).report.counts().added, 1);
    assert_same(&state(&fx.ed.model, true), &s1, "a sync writes nothing");
    fx.ed.undo();
    assert_same(&state(&fx.ed.model, true), &s0, "undo of the delete: every table as before it");
    assert_eq!(made(&fx.ed, &p), 1, "the record is back with E");
    let again = sync(&mut fx.ed, &p);
    assert_eq!(again.report.counts().added, 0, "the row is not offered again: {:?}", again.report.changes);
    assert_same(&state(&fx.ed.model, true), &s0, "the sync after undo writes nothing");
}

/// E deleted, then an unrelated new wall gets E's id: the book row is offered again (not
/// skipped as present), and applying it creates it.
#[test]
fn xlsx_reused_id_does_not_hide_the_row() {
    let mut fx = scene();
    let (p, e) = applied_new_wall(&mut fx, "reuse");
    delete_wall(&mut fx.ed, e);
    let (level, wt) = (fx.ed.level, ids(&fx.ed.model, "wall_type")[0]);
    let spec = WallSpec { a: (0.0, 30.0), b: (3.0, 30.0), level, base_offset: 0.0, height: 3.0, wall_type: wt, offset0: None };
    fx.ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.add_wall(&spec).map(drop)))).unwrap();
    assert!(ids(&fx.ed.model, "wall").contains(&e), "fixture: the new wall reuses E's id");
    assert_eq!(made(&fx.ed, &p), 0, "the unrelated wall does not claim the row");
    let pv = sync(&mut fx.ed, &p);
    assert_eq!(pv.report.counts().added, 1, "the row is offered: {:?}", pv.report.changes);
    let walls = ids(&fx.ed.model, "wall").len();
    apply(&mut fx.ed, &pv);
    assert_eq!(ids(&fx.ed.model, "wall").len(), walls + 1, "the row's wall is made again");
}
