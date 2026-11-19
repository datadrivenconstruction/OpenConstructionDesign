//! A sheet that is missing, renamed, cleared, left with only its header or unreadable: the report
//! names it with the reason, its elements stay and the other sheets still apply.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, WallSpec};
use rust_xlsxwriter::Workbook;

use super::tests::{edit, set, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;

const ELEVATION: &str = "elevation [m]";
const HEIGHT: &str = "height [m]";

fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layer = LayerSpec { material: "brick".into(), thickness: 0.2 };
    let t = m.add_wall_type("ext", &[layer]).unwrap();
    for (a, b) in [((0., 0.), (5., 0.)), ((5., 0.), (5., 4.))] {
        m.add_wall(&WallSpec {
            a,
            b,
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap();
    }
    m
}

fn exported(m: &Model, tag: &str) -> PathBuf {
    let p = tmp(&format!("xs66_{tag}_src"));
    export_xlsx(m, &p).unwrap();
    p
}

fn walls(m: &Model) -> i64 {
    m.sql().query_row("SELECT COUNT(*) FROM wall", [], |r| r.get(0)).unwrap()
}

fn wall_height(m: &Model) -> f64 {
    m.sql().query_row("SELECT height FROM wall ORDER BY id", [], |r| r.get(0)).unwrap()
}

fn elevation(m: &Model) -> f64 {
    m.sql().query_row("SELECT elevation FROM level", [], |r| r.get(0)).unwrap()
}

fn with_delete(apply: bool) -> ImportOptions {
    ImportOptions { apply, delete_missing: true, new_params: false }
}

fn raise_level(g: &mut Grid) {
    set(g, 1, ELEVATION, Data::Float(0.5));
}

/// A copy of `src` with the walls sheet renamed (`Some`) or dropped (`None`), the level raised.
fn without_walls(src: &Path, dst: &Path, to: Option<&str>, drop_base: bool) {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for name in wb.sheet_names() {
        let mut g: Grid = wb.worksheet_range(&name).unwrap().rows().map(|r| r.to_vec()).collect();
        if name == WALLS && to.is_none() {
            continue;
        }
        if name == LEVELS {
            raise_level(&mut g);
        }
        if name == BASELINE && drop_base {
            g.clear();
        }
        let ws = out.add_worksheet();
        ws.set_name(if name == WALLS { to.unwrap() } else { name.as_str() }).unwrap();
        for (i, row) in g.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                    Data::String(t) => ws.write_string(i, j, t).map(|_| ()),
                    Data::Bool(b) => ws.write_boolean(i, j, *b).map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(dst).unwrap();
}

/// A copy of `src` with the xml of `sheet` passed through `f`; everything else byte for byte.
fn patch_sheet_xml(src: &Path, dst: &Path, sheet: &str, f: impl Fn(String) -> String) {
    let wb: Xlsx<_> = open_workbook(src).unwrap();
    let at = wb.sheet_names().iter().position(|n| n == sheet).unwrap();
    let part = format!("xl/worksheets/sheet{}.xml", at + 1);
    let mut zin = zip::ZipArchive::new(std::fs::File::open(src).unwrap()).unwrap();
    let mut zout = zip::ZipWriter::new(std::fs::File::create(dst).unwrap());
    let o = zip::write::SimpleFileOptions::default();
    let mut hit = false;
    for i in 0..zin.len() {
        let mut e = zin.by_index(i).unwrap();
        let name = e.name().unwrap().into_owned();
        let mut buf = vec![];
        e.read_to_end(&mut buf).unwrap();
        if name == part {
            buf = f(String::from_utf8(buf).unwrap()).into_bytes();
            hit = true;
        }
        zout.start_file(name, o).unwrap();
        zout.write_all(&buf).unwrap();
    }
    zout.finish().unwrap();
    assert!(hit, "no part {part}");
}

/// Common verdict: the preview names the walls sheet alone, the model is not touched, and the
/// Apply takes the level edit while no wall is deleted or changed.
fn named_and_kept(m: &mut Model, q: &Path, kind: SheetIssueKind, kept: usize) {
    let (h0, e0) = (wall_height(m), elevation(m));
    let pre = import_xlsx_with(m, q, with_delete(false)).unwrap();
    assert_eq!(pre.sheet_issues, [SheetIssue::new(WALLS, kind, kept)], "{pre:?}");
    assert_eq!(pre.blocking(), 0, "the other sheets stay available: {pre:?}");
    assert!(pre.changes.iter().all(|c| c.sheet != WALLS), "{:?}", pre.changes);
    assert!(pre.removed.is_empty() && pre.deletions.is_empty(), "{pre:?}");
    assert_eq!(pre.counts().deleted, 0, "{pre:?}");
    assert!(pre.changes.iter().any(|c| c.sheet == LEVELS), "the level edit shows: {pre:?}");
    assert_eq!((walls(m), elevation(m)), (2, e0), "a preview writes nothing");
    let done = import_xlsx_with(m, q, with_delete(true)).unwrap();
    assert!(done.applied, "{done:?}");
    assert_eq!(walls(m), 2, "the book deleted nothing");
    assert!((wall_height(m) - h0).abs() < 1e-12, "no wall edit");
    assert!((elevation(m) - 0.5).abs() < 1e-9, "the other sheet applied");
}

#[test]
fn missing_sheet_is_named_and_deletes_nothing() {
    let mut m = model();
    let p = exported(&m, "gone");
    let q = tmp("xs66_gone");
    without_walls(&p, &q, None, false);
    named_and_kept(&mut m, &q, SheetIssueKind::Missing, 0);
}

#[test]
fn renamed_sheet_is_named_and_deletes_nothing() {
    let mut m = model();
    let p = exported(&m, "ren");
    let q = tmp("xs66_ren");
    without_walls(&p, &q, Some("Walls old"), false);
    named_and_kept(&mut m, &q, SheetIssueKind::Missing, 0);
}

#[test]
fn cleared_sheet_is_named_and_deletes_nothing() {
    let mut m = model();
    let p = exported(&m, "clr");
    let q = tmp("xs66_clr");
    edit(&p, &q, |sh, g| {
        match sh {
            WALLS => g.clear(),
            LEVELS => raise_level(g),
            _ => {}
        }
    });
    named_and_kept(&mut m, &q, SheetIssueKind::Empty, 2);
}

#[test]
fn sheet_of_blank_text_counts_as_cleared() {
    let mut m = model();
    let p = exported(&m, "blk");
    let q = tmp("xs66_blk");
    edit(&p, &q, |sh, g| {
        match sh {
            WALLS => *g = vec![vec![Data::String("  ".into())]],
            LEVELS => raise_level(g),
            _ => {}
        }
    });
    named_and_kept(&mut m, &q, SheetIssueKind::Empty, 2);
}

#[test]
fn header_only_sheet_is_named_and_deletes_nothing() {
    let mut m = model();
    let p = exported(&m, "hdr");
    let q = tmp("xs66_hdr");
    edit(&p, &q, |sh, g| {
        match sh {
            WALLS => g.truncate(1),
            LEVELS => raise_level(g),
            _ => {}
        }
    });
    named_and_kept(&mut m, &q, SheetIssueKind::Empty, 2);
}

#[test]
fn header_with_only_blank_rows_counts_as_header_only() {
    let mut m = model();
    let p = exported(&m, "hdb");
    let q = tmp("xs66_hdb");
    edit(&p, &q, |sh, g| {
        match sh {
            WALLS => {
                for row in g.iter_mut().skip(1) {
                    row.iter_mut().for_each(|c| *c = Data::String(" ".into()));
                }
            }
            LEVELS => raise_level(g),
            _ => {}
        }
    });
    named_and_kept(&mut m, &q, SheetIssueKind::Empty, 2);
}

#[test]
fn header_only_with_a_wrong_header_reports_only_the_empty_sheet() {
    let mut m = model();
    let p = exported(&m, "hdw");
    let q = tmp("xs66_hdw");
    edit(&p, &q, |sh, g| {
        match sh {
            WALLS => {
                g.truncate(1);
                let c = super::tests::col(g, HEIGHT);
                g[0][c] = Data::String("something else".into());
            }
            LEVELS => raise_level(g),
            _ => {}
        }
    });
    named_and_kept(&mut m, &q, SheetIssueKind::Empty, 2);
}

#[test]
fn sheet_exported_without_rows_is_no_issue() {
    let mut m = model();
    let p = exported(&m, "norooms");
    let rep = import_xlsx_with(&mut m, &p, with_delete(false)).unwrap();
    assert!(rep.sheet_issues.is_empty(), "{:?}", rep.sheet_issues);
    assert_eq!(rep.counts().deleted, 0);
}

#[test]
fn a_kept_row_still_deletes_the_others() {
    let mut m = model();
    let p = exported(&m, "one");
    let q = tmp("xs66_one");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            g.pop();
        }
    });
    let rep = import_xlsx_with(&mut m, &q, with_delete(true)).unwrap();
    assert!(rep.sheet_issues.is_empty(), "{:?}", rep.sheet_issues);
    assert_eq!((rep.counts().deleted, walls(&m)), (1, 1), "{rep:?}");
}

#[test]
fn damaged_sheet_xml_is_named_and_the_others_apply() {
    let mut m = model();
    let p = exported(&m, "bad");
    let e = tmp("xs66_bad_e");
    edit(&p, &e, |sh, g| {
        if sh == LEVELS {
            raise_level(g);
        }
    });
    let q = tmp("xs66_bad");
    patch_sheet_xml(&e, &q, WALLS, |x| {
        let cut = x.find("<row r=\"2\"").expect("a data row");
        x[..cut + 4].to_string()
    });
    named_and_kept(&mut m, &q, SheetIssueKind::Unreadable, 2);
}

#[test]
fn unreadable_formulas_refuse_the_sheet_instead_of_reading_values_as_edits() {
    let mut m = model();
    let p = exported(&m, "frm");
    let e = tmp("xs66_frm_e");
    edit(&p, &e, |sh, g| {
        match sh {
            WALLS => set(g, 1, HEIGHT, Data::Float(9.0)),
            LEVELS => raise_level(g),
            _ => {}
        }
    });
    // the same book with readable formulas does take the edit: the refusal below is what stops it
    let clean = import_xlsx_with(&mut m, &e, with_delete(false)).unwrap();
    assert!(clean.changes.iter().any(|c| c.sheet == WALLS), "{clean:?}");
    let q = tmp("xs66_frm");
    // a shared formula with no index: the values parse, the formulas do not
    patch_sheet_xml(&e, &q, WALLS, |x| {
        let row = x.find("<row r=\"2\"").expect("a data row");
        let v = row + x[row..].find("<v>").expect("a value");
        format!("{}<f t=\"shared\">A1</f>{}", &x[..v], &x[v..])
    });
    named_and_kept(&mut m, &q, SheetIssueKind::FormulasUnreadable, 2);
}

#[test]
fn unreadable_sheet_gives_no_id_column() {
    let mut m = model();
    let p = exported(&m, "idl");
    let q = tmp("xs66_idl");
    patch_sheet_xml(&p, &q, WALLS, |x| {
        let cut = x.find("<row r=\"2\"").expect("a data row");
        x[..cut + 4].to_string()
    });
    let mut wb: Xlsx<_> = open_workbook(&q).unwrap();
    let alias = std::collections::HashMap::new();
    assert!(import::id_column(&mut wb, WALLS, WALLS_COLS, &alias).is_none());
}

#[test]
fn without_baseline_missing_and_header_only_sheets_are_still_named() {
    let mut m = model();
    let p = exported(&m, "nob");
    let q = tmp("xs66_nob");
    without_walls(&p, &q, None, true);
    let rep = import_xlsx_with(&mut m, &q, with_delete(false)).unwrap();
    assert!(rep.no_base, "{rep:?}");
    let named: Vec<(&str, SheetIssueKind)> =
        rep.sheet_issues.iter().map(|i| (i.sheet.as_str(), i.kind)).collect();
    assert_eq!(named, [(WALLS, SheetIssueKind::Missing)]);

    let q = tmp("xs66_nob_hdr");
    edit(&p, &q, |sh, g| {
        match sh {
            BASELINE => g.clear(),
            WALLS => g.truncate(1),
            _ => {}
        }
    });
    let rep = import_xlsx_with(&mut m, &q, with_delete(false)).unwrap();
    assert!(rep.no_base, "{rep:?}");
    assert_eq!(rep.sheet_issues, [SheetIssue::new(WALLS, SheetIssueKind::Empty, 2)]);
    assert!(rep.removed.is_empty() && rep.counts().deleted == 0, "{rep:?}");
    assert_eq!(walls(&m), 2);
}

#[test]
fn kept_counts_only_what_the_model_still_holds() {
    let mut m = model();
    let p = exported(&m, "gonew");
    let first: i64 = m.sql().query_row("SELECT MIN(id) FROM wall", [], |r| r.get(0)).unwrap();
    m.delete_wall(first).unwrap();
    let q = tmp("xs66_gonew");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            g.truncate(1);
        }
    });
    let rep = import_xlsx_with(&mut m, &q, with_delete(false)).unwrap();
    assert_eq!(rep.sheet_issues, [SheetIssue::new(WALLS, SheetIssueKind::Empty, 1)], "{rep:?}");
    assert_eq!(walls(&m), 1);
}

#[test]
fn kept_counts_exported_ids_not_a_new_element() {
    let mut m = model();
    let p = exported(&m, "swap");
    let q = |s: &str| -> i64 { m.sql().query_row(s, [], |r| r.get(0)).unwrap() };
    let (first, t) = (q("SELECT MIN(id) FROM wall"), q("SELECT MIN(wtype) FROM wall"));
    let level = q("SELECT MIN(level) FROM wall");
    m.delete_wall(first).unwrap();
    let spec = WallSpec {
        a: (0., 9.),
        b: (6., 9.),
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: t,
        offset0: None,
    };
    m.add_wall(&spec).unwrap();
    let q = tmp("xs66_swap");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            g.truncate(1);
        }
    });
    // two walls now, one of them new: the sheet stood for the one exported wall still there;
    // Apply does what the preview said
    for apply in [false, true] {
        let rep = import_xlsx_with(&mut m, &q, with_delete(apply)).unwrap();
        assert_eq!(rep.sheet_issues, [SheetIssue::new(WALLS, SheetIssueKind::Empty, 1)], "{rep:?}");
        assert!(rep.removed.is_empty() && rep.counts().deleted == 0, "{rep:?}");
    }
    assert_eq!(walls(&m), 2);
}

/// Copy of `src` without its baseline and `drop_sheet`; Info rows keyed in `cut` are gone.
fn old_book(src: &Path, dst: &Path, drop_sheet: &str, cut: &[&str]) {
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for name in wb.sheet_names().into_iter().filter(|n| n != drop_sheet) {
        let mut g: Grid = wb.worksheet_range(&name).unwrap().rows().map(|r| r.to_vec()).collect();
        match name.as_str() {
            BASELINE => g.clear(),
            INFO => g.retain(|r| !r.first().is_some_and(|k| cut.contains(&k.to_string().as_str()))),
            _ => {}
        }
        let ws = out.add_worksheet();
        ws.set_name(&name).unwrap();
        for (i, row) in g.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                    Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                    Data::Bool(b) => ws.write_boolean(i, j, *b).map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(dst).unwrap();
}

fn with_curtain() -> Model {
    let mut m = model();
    let level: i64 = m.sql().query_row("SELECT MIN(id) FROM level", [], |r| r.get(0)).unwrap();
    let sec = ddc_model::MullionSection::Rect { w: 0.05, d: 0.1 };
    m.add_curtain_wall(&ddc_model::CurtainSpec {
        level,
        name: "CW".into(),
        a: (0.0, 6.0),
        b: (4.0, 6.0),
        bulge: 0.0,
        base_offset: 0.0,
        height: 3.0,
        u: ddc_model::GridLines::Spacing(1.0),
        v: ddc_model::GridLines::Spacing(1.5),
        interior: sec,
        border: sec,
        lines: Default::default(),
        top: vec![],
    })
    .unwrap();
    m
}

#[test]
fn without_baseline_only_sheets_that_book_had_are_named() {
    let mut m = with_curtain();
    let p = exported(&m, "age");
    let mut missing = |tag: &str, sheet: &str, cut: &[&str]| -> Vec<String> {
        let q = tmp(&format!("xs66_age_{tag}"));
        old_book(&p, &q, sheet, cut);
        let rep = import_xlsx_with(&mut m, &q, with_delete(false)).unwrap();
        assert!(rep.no_base && rep.counts().deleted == 0, "{rep:?}");
        let gone = rep.sheet_issues.iter().filter(|i| i.kind == SheetIssueKind::Missing);
        gone.map(|i| i.sheet.clone()).collect()
    };
    // a current book lists what its export wrote
    assert_eq!(missing("cur", CURTAINS, &[]), [CURTAINS]);
    // an older one goes by its Info rows: a revision row came after the curtain sheet
    assert_eq!(missing("rev", CURTAINS, &[no_base::SHEETS_ROW]), [CURTAINS]);
    let old = [no_base::SHEETS_ROW, "revision"];
    assert!(missing("old", CURTAINS, &old).is_empty());
    // walls were in every book with an Info sheet
    assert_eq!(missing("oldw", WALLS, &old), [WALLS]);
    assert_eq!(walls(&m), 2);
}
