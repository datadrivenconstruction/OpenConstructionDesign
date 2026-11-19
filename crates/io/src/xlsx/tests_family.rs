// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::path::PathBuf;

use calamine::Data;
use ddc_model::{CeilingSpec, Error, LayerSpec, Model};

use super::tests::{col, edit, set, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;

const SHEET: &str = "Ceilings";
const RING: [(f64, f64); 4] = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)];

fn get_h(m: &Model, id: i64) -> Result<f64, Error> {
    Ok(m.sql()
        .query_row("SELECT height FROM ceiling WHERE id=?1", [id], |r| r.get(0))?)
}

fn set_h(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let (level, slab_type): (i64, i64) = m.sql().query_row(
        "SELECT level,stype FROM ceiling WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let s = CeilingSpec {
        level,
        slab_type,
        height: v,
        outer: RING.to_vec(),
        holes: vec![],
    };
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
    let layer = LayerSpec {
        material: "plaster".into(),
        thickness: 0.02,
    };
    let st = m.add_slab_type("ceiling", &[layer]).unwrap();
    let ids = [2.75, 2.6]
        .map(|h| {
            let s = CeilingSpec {
                level: lv,
                slab_type: st,
                height: h,
                outer: RING.to_vec(),
                holes: vec![],
            };
            m.add_ceiling(&s).unwrap()
        })
        .to_vec();
    (m, ids)
}

fn opts(apply: bool) -> ImportOptions {
    ImportOptions {
        apply,
        delete_missing: false,
        new_params: false,
    }
}

fn ops(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT COUNT(*) FROM op", [], |r| r.get(0))
        .unwrap()
}

fn book(m: &Model, name: &str, u: Units) -> PathBuf {
    let p = tmp(name);
    export_xlsx_families(m, &p, u, &fams()).unwrap();
    p
}

fn edited(src: &PathBuf, name: &str, f: impl Fn(&mut Grid)) -> PathBuf {
    let q = tmp(name);
    edit(src, &q, |s, g| {
        if s == SHEET {
            f(g)
        }
    });
    q
}

#[test]
fn unedited_family_sheet_has_no_changes() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_same", Units::Metric);
    assert_eq!(sheet_ids(&p, SHEET).unwrap(), ids);
    let rep = import_xlsx_families(&mut m, &p, opts(true), &fams()).unwrap();
    assert!(rep.applied && rep.changes.is_empty() && rep.props.is_empty(), "{rep:?}");
}

#[test]
fn cell_edit_previews_then_applies_as_one_undo_step() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_edit_src", Units::Metric);
    let q = edited(&p, "fam_edit", |g| set(g, 2, "Height [m]", Data::Float(3.1)));
    let (n0, h0) = (ops(&m), get_h(&m, ids[1]).unwrap());
    let rep = import_xlsx_families(&mut m, &q, opts(false), &fams()).unwrap();
    assert!(rep.rejected.is_empty() && !rep.applied, "{rep:?}");
    assert_eq!(rep.props.len(), 1);
    assert_eq!((rep.changes[0].sheet.as_str(), rep.changes[0].row), (SHEET, 3));
    assert_eq!(rep.changes[0].id, Some(ids[1]));
    assert_eq!((ops(&m), get_h(&m, ids[1]).unwrap()), (n0, h0));
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!((get_h(&m, ids[1]).unwrap() - 3.1).abs() < 1e-12);
    assert_eq!(get_h(&m, ids[0]).unwrap(), 2.75);
    assert_eq!(ops(&m) - n0, 1);
    m.undo().unwrap();
    assert_eq!((ops(&m), get_h(&m, ids[1]).unwrap()), (n0, h0));
}

#[test]
fn imperial_book_converts_and_text_lengths_are_read() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_imp_src", Units::Imperial);
    let head = "Height [ft]";
    let q = edited(&p, "fam_imp", |g| {
        set(g, 1, head, Data::Float(10.0));
        set(g, 2, head, Data::String("3500 mm".into()));
    });
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!((get_h(&m, ids[0]).unwrap() - 3.048).abs() < 1e-9);
    assert!((get_h(&m, ids[1]).unwrap() - 3.5).abs() < 1e-9);
}

#[test]
fn bad_rows_are_reported_per_row_and_skipped() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_bad_src", Units::Metric);
    let q = edited(&p, "fam_bad", |g| {
        set(g, 1, "Height [m]", Data::Float(-5.0));
        set(g, 2, "id", Data::Float(999.0));
        let mut extra = vec![Data::Empty; g[0].len()];
        extra[col(g, "id")] = Data::Float(ids[0] as f64);
        extra[col(g, "Height [m]")] = Data::String("tall".into());
        g.push(extra);
    });
    let before: Vec<f64> = ids.iter().map(|i| get_h(&m, *i).unwrap()).collect();
    let n0 = ops(&m);
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(rep.applied && rep.blocking() == 0, "{rep:?}");
    let at = |row| rep.rejected.iter().find(|r| r.row == row).unwrap();
    assert!(at(2).reason.contains("between"), "{:?}", at(2));
    assert_eq!((at(2).sheet.as_str(), at(2).column.as_str()), (SHEET, "Height [m]"));
    let ig = |row| rep.ignored.iter().find(|r| r.row == row).unwrap();
    assert!(ig(3).reason.contains("ceiling 999 no longer exists"), "{:?}", ig(3));
    assert!(ig(4).reason.contains("duplicate id"), "{:?}", ig(4));
    assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
    let after: Vec<f64> = ids.iter().map(|i| get_h(&m, *i).unwrap()).collect();
    assert_eq!((after, ops(&m)), (before, n0));
}

#[test]
fn text_where_a_number_is_expected_is_rejected() {
    let (mut m, _) = model();
    let p = book(&m, "fam_txt_src", Units::Metric);
    let q = edited(&p, "fam_txt", |g| {
        set(g, 1, "Height [m]", Data::String("tall".into()))
    });
    let rep = import_xlsx_families(&mut m, &q, opts(false), &fams()).unwrap();
    assert_eq!(rep.rejected.len(), 1, "{rep:?}");
    assert!(rep.rejected[0].reason.contains("not a number or length"));
    assert_eq!(rep.rejected[0].address(), "Ceilings!C2");
}

#[test]
fn unknown_columns_new_rows_and_read_only_edits_are_never_silent() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_misc_src", Units::Metric);
    let q = edited(&p, "fam_misc", |g| {
        g[0].push(Data::String("my note".into()));
        g[1].push(Data::String("keep".into()));
        let mut fresh = vec![Data::Empty; g[0].len()];
        fresh[col(g, "Height [m]")] = Data::Float(3.0);
        g.push(fresh);
        set(g, 2, "level (read-only)", Data::String("Other".into()));
    });
    let rep = import_xlsx_families(&mut m, &q, opts(false), &fams()).unwrap();
    assert!(rep.ignored.iter().any(|i| i.column == "my note"), "{rep:?}");
    assert!(
        rep.ignored
            .iter()
            .any(|i| i.row == 4 && i.reason.contains("no id")),
        "{rep:?}"
    );
    assert!(rep.rejected.is_empty(), "{rep:?}");
    let ro = rep.ignored.iter().find(|i| i.row == 3).unwrap();
    assert!(ro.reason.contains("read-only"), "{ro:?}");
    assert_eq!(get_h(&m, ids[0]).unwrap(), 2.75);
}

#[test]
fn renamed_caption_named_in_report() {
    let (mut m, _) = model();
    let p = book(&m, "fam_ro_src", Units::Metric);
    let q = edited(&p, "fam_ro", |g| {
        set(g, 1, "level (read-only)", Data::String("Other".into()));
    });
    let rep = import_xlsx_families(&mut m, &q, opts(false), &fams()).unwrap();
    let ro = rep.ignored.iter().find(|i| i.reason.contains("read-only")).unwrap();
    assert_eq!(ro.column, "level (read-only)", "{ro:?}");
    assert_eq!(column_label("Floor", "level (read-only)"), "Floor [level (read-only)]");
}

#[test]
fn reexport_replaces_the_family_sheet_without_duplicating_it() {
    let (m, ids) = model();
    let p = book(&m, "fam_again", Units::Metric);
    export_xlsx_families(&m, &p, Units::Metric, &fams()).unwrap();
    assert_eq!(sheet_ids(&p, SHEET).unwrap(), ids);
}

fn ceilings(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT COUNT(*) FROM ceiling", [], |r| r.get(0))
        .unwrap()
}

fn text_grid(path: &PathBuf, sheet: &str) -> Vec<Vec<String>> {
    use calamine::Reader;
    let mut wb: calamine::Xlsx<_> = calamine::open_workbook(path).unwrap();
    wb.worksheet_range(sheet)
        .unwrap()
        .rows()
        .map(|r| r.iter().map(|d| d.to_string()).collect())
        .collect()
}

#[test]
fn delete_flag_previews_then_applies_as_one_undo_step() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_del_src", Units::Metric);
    let q = edited(&p, "fam_del", |g| {
        set(g, 2, "delete", Data::String("delete".into()))
    });
    let n0 = ops(&m);
    let rep = import_xlsx_families(&mut m, &q, opts(false), &fams()).unwrap();
    assert!(rep.rejected.is_empty() && !rep.applied, "{rep:?}");
    assert_eq!(rep.counts().deleted, 1, "{rep:?}");
    assert_eq!(rep.changes[0].id, Some(ids[1]));
    assert_eq!((ceilings(&m), ops(&m)), (2, n0));
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(ceilings(&m), 1);
    assert_eq!(ops(&m) - n0, 1);
    m.undo().unwrap();
    assert_eq!((ceilings(&m), ops(&m)), (2, n0));
}

#[test]
fn an_edit_and_a_delete_apply_together_and_undo_together() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_mix_src", Units::Metric);
    let q = edited(&p, "fam_mix", |g| {
        set(g, 1, "Height [m]", Data::Float(3.3));
        set(g, 2, "delete", Data::String("x".into()));
    });
    let n0 = ops(&m);
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(ceilings(&m), 1);
    assert!((get_h(&m, ids[0]).unwrap() - 3.3).abs() < 1e-12);
    for _ in n0..ops(&m) {
        m.undo().unwrap();
    }
    assert_eq!(ceilings(&m), 2);
    assert_eq!(get_h(&m, ids[0]).unwrap(), 2.75);
}

#[test]
fn bad_delete_flag_is_skipped_and_the_element_stays() {
    let (mut m, _) = model();
    let p = book(&m, "fam_flag_src", Units::Metric);
    let q = edited(&p, "fam_flag", |g| {
        set(g, 1, "delete", Data::String("maybe".into()))
    });
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(rep.applied && rep.rejected.len() == 1, "{rep:?}");
    assert!(rep.rejected[0].reason.contains("delete flag"), "{rep:?}");
    assert_eq!(ceilings(&m), 2);
}

#[test]
fn a_refused_family_rejects_a_delete_flag_per_row_and_keeps_the_element() {
    const WHY: &str = "annotations are deleted in the editor";
    let (mut m, _) = model();
    let mut f = fams();
    f[0].delete = RowDelete::Refused(WHY);
    let p = tmp("fam_ref_src");
    export_xlsx_families(&m, &p, Units::Metric, &f).unwrap();
    assert!(!text_grid(&p, SHEET)[0].iter().any(|h| h == DELETE_HEAD));
    let q = edited(&p, "fam_ref", |g| {
        g[0].push(Data::String(DELETE_HEAD.into()));
        set(g, 2, DELETE_HEAD, Data::String("delete".into()));
    });
    let rep = import_xlsx_families(&mut m, &q, opts(true), &f).unwrap();
    assert!(rep.applied && rep.rejected.len() == 1, "{rep:?}");
    assert_eq!((rep.rejected[0].row, rep.rejected[0].reason.as_str()), (3, WHY));
    assert_eq!(ceilings(&m), 2);
}

#[test]
fn rows_removed_from_the_sheet_delete_only_when_asked() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_miss_src", Units::Metric);
    let q = edited(&p, "fam_miss", |g| {
        g.remove(2);
    });
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(rep.applied && ceilings(&m) == 2, "{rep:?}");
    let ask = ImportOptions { apply: false, delete_missing: true, new_params: false };
    let rep = import_xlsx_families(&mut m, &q, ask, &fams()).unwrap();
    assert_eq!(rep.counts().deleted, 1, "{rep:?}");
    assert_eq!(rep.changes[0].id, Some(ids[1]));
    let ask = ImportOptions { apply: true, ..ask };
    let rep = import_xlsx_families(&mut m, &q, ask, &fams()).unwrap();
    assert!(rep.applied && ceilings(&m) == 1, "{rep:?}");
    m.undo().unwrap();
    assert_eq!(ceilings(&m), 2);
}

fn bad_id_keeps_elements(tag: &str, bad: Data) {
    let (mut m, _) = model();
    let p = book(&m, &format!("{tag}_src"), Units::Metric);
    let q = edited(&p, tag, |g| set(g, 2, "id", bad.clone()));
    let ask = ImportOptions { apply: true, delete_missing: true, new_params: false };
    let rep = import_xlsx_families(&mut m, &q, ask, &fams()).unwrap();
    // the row is rejected and deletes are skipped; the book itself is not blocked
    assert!(rep.applied && rep.blocking() == 0, "{rep:?}");
    assert!(rep.changes.iter().all(|c| c.kind() != "deleted"), "{rep:?}");
    assert!(rep.rejected.iter().any(|r| r.reason == super::import::NO_DELETES), "{rep:?}");
    assert_eq!(ceilings(&m), 2);
}

#[test]
fn family_bad_id_row_not_deleted_with_delete_missing() {
    bad_id_keeps_elements("fam_badid", Data::String("7x".into()));
}

#[test]
fn family_float_id_row_not_deleted_with_delete_missing() {
    bad_id_keeps_elements("fam_floatid", Data::Float(5.5));
}

fn wiped_family_sheet_is_named_and_deletes_nothing(tag: &str, wipe: fn(&mut Grid)) {
    let (mut m, _) = model();
    let p = book(&m, &format!("{tag}_src"), Units::Metric);
    let q = edited(&p, tag, wipe);
    let ask = ImportOptions { apply: true, delete_missing: true, new_params: false };
    let rep = import_xlsx_families(&mut m, &q, ask, &fams()).unwrap();
    assert!(rep.applied && rep.blocking() == 0, "{rep:?}");
    assert_eq!(rep.sheet_issues, [SheetIssue::new(SHEET, SheetIssueKind::Empty, 2)]);
    assert!(rep.removed.is_empty() && rep.counts().deleted == 0, "{rep:?}");
    assert_eq!(ceilings(&m), 2);
}

#[test]
fn cleared_family_sheet_is_named_and_deletes_nothing() {
    wiped_family_sheet_is_named_and_deletes_nothing("fam_cleared", |g| g.clear());
}

#[test]
fn header_only_family_sheet_is_named_and_deletes_nothing() {
    wiped_family_sheet_is_named_and_deletes_nothing("fam_header", |g| g.truncate(1));
}

#[test]
fn family_good_ids_still_delete_missing_rows() {
    let (mut m, _) = model();
    let p = book(&m, "fam_goodid_src", Units::Metric);
    let q = edited(&p, "fam_goodid", |g| {
        g.remove(2);
    });
    let ask = ImportOptions { apply: true, delete_missing: true, new_params: false };
    let rep = import_xlsx_families(&mut m, &q, ask, &fams()).unwrap();
    assert!(rep.applied && rep.blocking() == 0 && ceilings(&m) == 1, "{rep:?}");
}

#[test]
fn user_columns_of_a_family_sheet_survive_reexport_by_id() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_user_src", Units::Metric);
    let q = edited(&p, "fam_user", |g| {
        g[0].push(Data::String("my note".into()));
        set(g, 1, "my note", Data::String("first".into()));
        set(g, 2, "my note", Data::String("second".into()));
    });
    export_xlsx_families(&m, &q, Units::Metric, &fams()).unwrap();
    let g = text_grid(&q, SHEET);
    let c = g[0].iter().position(|h| h == "my note").expect("user column kept");
    assert_eq!((g[1][c].as_str(), g[2][c].as_str()), ("first", "second"));
    // the note follows its element when the rows above it go away
    let ceil = ids[0];
    m.delete_items("t", &[("ceiling", ceil)]).unwrap();
    export_xlsx_families(&m, &q, Units::Metric, &fams()).unwrap();
    let g = text_grid(&q, SHEET);
    let c = g[0].iter().position(|h| h == "my note").expect("user column kept");
    assert_eq!((g.len(), g[1][c].as_str()), (2, "second"));
    // the format's own columns are never taken for user columns
    assert_eq!(g[0].iter().filter(|h| h.starts_with("Height")).count(), 1);
    assert_eq!(g[0].iter().filter(|h| *h == DELETE_HEAD).count(), 1);
}

#[test]
fn family_cell_edited_in_book_and_model_is_a_conflict() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_conf_src", Units::Metric);
    let q = edited(&p, "fam_conf", |g| {
        set(g, 1, "Height [m]", Data::Float(3.9));
        set(g, 2, "Height [m]", Data::Float(3.1));
    });
    set_h(&mut m, ids[0], 3.3).unwrap();
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(rep.conflicts.len(), 1, "{:?}", rep.conflicts);
    let c = rep.conflicts[0].clone();
    assert_eq!((c.sheet.as_str(), c.row, c.id), (SHEET, 2, Some(ids[0])));
    assert_eq!((c.base.as_str(), c.book.as_str(), c.model.as_str()), ("2.75", "3.9", "3.3"));
    assert!((get_h(&m, ids[0]).unwrap() - 3.3).abs() < 1e-12, "the model value stays");
    assert!((get_h(&m, ids[1]).unwrap() - 3.1).abs() < 1e-12, "the book-only edit applies");
    // taking the book value applies it, in one undo step
    let take = Take {
        all: false,
        cells: [(c.sheet.clone(), c.row, c.col)].into_iter().collect(),
        ..Take::default()
    };
    let n0 = ops(&m);
    let rep = import_xlsx_families_take(&mut m, &q, opts(true), &fams(), &take).unwrap();
    assert!(rep.applied && rep.conflicts[0].take_book, "{rep:?}");
    assert!((get_h(&m, ids[0]).unwrap() - 3.9).abs() < 1e-12);
    assert_eq!(ops(&m) - n0, 1);
    m.undo().unwrap();
    assert!((get_h(&m, ids[0]).unwrap() - 3.3).abs() < 1e-12);
}

#[test]
fn family_cell_untouched_in_book_keeps_a_later_model_edit() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_newer_src", Units::Metric);
    set_h(&mut m, ids[1], 2.2).unwrap();
    let rep = import_xlsx_families(&mut m, &p, opts(true), &fams()).unwrap();
    assert!(rep.applied && rep.changes.is_empty() && rep.conflicts.is_empty(), "{rep:?}");
    assert_eq!(rep.model_newer, 1);
    assert!((get_h(&m, ids[1]).unwrap() - 2.2).abs() < 1e-12);
}

#[test]
fn blank_family_cell_is_skipped_alone() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_blank_src", Units::Metric);
    let q = edited(&p, "fam_blank", |g| {
        set(g, 1, "Height [m]", Data::Empty);
        set(g, 2, "Height [m]", Data::Float(3.1));
    });
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(rep.applied && rep.blocking() == 0 && rep.skipped() == 1, "{rep:?}");
    assert!((get_h(&m, ids[0]).unwrap() - 2.75).abs() < 1e-12);
    assert!((get_h(&m, ids[1]).unwrap() - 3.1).abs() < 1e-12);
}

fn add_ceiling(m: &mut Model) -> i64 {
    let lv = m.add_level("L9", 9.0).unwrap();
    let layer = LayerSpec {
        material: "plaster".into(),
        thickness: 0.02,
    };
    let st = m.add_slab_type("ceiling9", &[layer]).unwrap();
    let s = CeilingSpec {
        level: lv,
        slab_type: st,
        height: 2.5,
        outer: RING.to_vec(),
        holes: vec![],
    };
    m.add_ceiling(&s).unwrap()
}

#[test]
fn family_sheet_conflict_detected() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_base_src", Units::Metric);
    // the family sheet has a baseline block with a row per element
    let mut wb: calamine::Xlsx<_> = calamine::open_workbook(&p).unwrap();
    let base = baseline::read(&mut wb).expect("baseline sheet");
    assert!(base.has_sheet(SHEET) && ids.iter().all(|i| base.has_id(SHEET, *i)));
    let q = edited(&p, "fam_base", |g| set(g, 1, "Height [m]", Data::Float(3.9)));
    set_h(&mut m, ids[0], 3.3).unwrap();
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert_eq!(rep.conflicts.len(), 1, "{:?}", rep.conflicts);
    assert_eq!(rep.kept.len(), 1, "an unresolved conflict is kept");
    assert!((get_h(&m, ids[0]).unwrap() - 3.3).abs() < 1e-12);
}

#[test]
fn created_after_export_not_deleted() {
    let (mut m, _) = model();
    let p = book(&m, "fam_new_src", Units::Metric);
    let fresh = add_ceiling(&mut m);
    let ask = ImportOptions { apply: true, delete_missing: true, new_params: false };
    let rep = import_xlsx_families(&mut m, &p, ask, &fams()).unwrap();
    assert!(rep.removed.is_empty() && rep.changes.is_empty(), "{rep:?}");
    assert_eq!(ceilings(&m), 3);
    assert!(get_h(&m, fresh).is_ok());
}

#[test]
fn reexport_keeps_conflict_cells() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_keep_src", Units::Metric);
    let q = edited(&p, "fam_keep", |g| set(g, 1, "Height [m]", Data::Float(3.9)));
    set_h(&mut m, ids[0], 3.3).unwrap();
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    let kept = rep.kept.clone();
    assert_eq!(kept.len(), 1);
    with_kept(&kept, || export_xlsx_families(&m, &q, Units::Metric, &fams())).unwrap();
    // the book cell still has the user's value and the next read finds the conflict again
    let g = text_grid(&q, SHEET);
    let c = g[0].iter().position(|h| h.starts_with("Height")).unwrap();
    assert_eq!(g[1][c].parse::<f64>().unwrap(), 3.9);
    let again = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert_eq!(again.conflicts.len(), 1, "{:?}", again.conflicts);
    assert_eq!(again.conflicts[0].base, "2.75");
    assert!((get_h(&m, ids[0]).unwrap() - 3.3).abs() < 1e-12);
    // without the keep list the cell is overwritten by the model value, as before
    export_xlsx_families(&m, &q, Units::Metric, &fams()).unwrap();
    let again = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(again.conflicts.is_empty() && again.kept.is_empty());
}

const TYPE_HEAD: &str = "Slab type (type id)";
const PICK_HEAD: &str = "Pick (choice 0-2)";

fn get_t(m: &Model, id: i64) -> Result<f64, Error> {
    Ok(m.sql()
        .query_row("SELECT stype FROM ceiling WHERE id=?1", [id], |r| r.get::<_, i64>(0))? as f64)
}

fn set_t(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let (level, height): (i64, f64) = m.sql().query_row(
        "SELECT level,height FROM ceiling WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let s = CeilingSpec {
        level,
        slab_type: v as i64,
        height,
        outer: RING.to_vec(),
        holes: vec![],
    };
    m.update_ceiling(id, &s)
}

fn fams_pick() -> Vec<FamilySheet> {
    let mut f = fams();
    let mut c = f[0].cols[0].clone();
    c.key = "pick";
    c.label = "Pick".into();
    c.dim = FamDim::Choice(3);
    c.labels = ["Left", "Centre", "Right"].map(String::from).to_vec();
    c.lo = 0.0;
    c.hi = 2.0;
    f[0].cols = vec![
        FamilyCol {
            key: "stype",
            label: "Slab type".into(),
            dim: FamDim::Type("slab_type"),
            labels: vec![],
            lo: 0.0,
            hi: 0.0,
            get: get_t,
            set: set_t,
        },
        c,
    ];
    f
}

/// Model with two ceilings of heights 1 and 2 (valid choice indexes) and a second type.
fn model_pick() -> (Model, Vec<i64>, i64) {
    let (mut m, ids) = model();
    m.sql().execute("UPDATE ceiling SET height=1.0 WHERE id=?1", [ids[0]]).unwrap();
    m.sql().execute("UPDATE ceiling SET height=2.0 WHERE id=?1", [ids[1]]).unwrap();
    let layer = LayerSpec {
        material: "plaster".into(),
        thickness: 0.02,
    };
    let rib = m.add_slab_type("Rib", &[layer]).unwrap();
    (m, ids, rib)
}

fn pick_book(m: &Model, name: &str) -> PathBuf {
    let p = tmp(name);
    export_xlsx_families(m, &p, Units::Metric, &fams_pick()).unwrap();
    p
}

fn cell_is(g: &Grid, row: usize, head: &str, want: &str) -> bool {
    matches!(&g[row][col(g, head)], Data::String(s) if s == want)
}

#[test]
fn family_type_cell_by_name_roundtrips() {
    let (mut m, ids, rib) = model_pick();
    let p = pick_book(&m, "fam_type_src");
    let q = edited(&p, "fam_type", |g| {
        assert!(cell_is(g, 1, TYPE_HEAD, "ceiling"), "{:?}", g[1]);
        assert!(cell_is(g, 1, PICK_HEAD, "Centre"), "{:?}", g[1]);
        set(g, 1, TYPE_HEAD, Data::String("  rIB ".into()));
    });
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams_pick()).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(get_t(&m, ids[0]).unwrap(), rib as f64);
    assert_eq!(rep.changes.len(), 1);
    assert_eq!(rep.changes[0].new, "Rib");
    // the next export shows the new name and reads back without changes
    let p2 = pick_book(&m, "fam_type_again");
    let again = import_xlsx_families(&mut m, &p2, opts(true), &fams_pick()).unwrap();
    assert!(again.props.is_empty() && again.rejected.is_empty(), "{again:?}");
}

#[test]
fn choice_label_imports_as_index() {
    let (mut m, ids, _) = model_pick();
    let p = pick_book(&m, "fam_pick_src");
    let q = edited(&p, "fam_pick", |g| {
        set(g, 1, PICK_HEAD, Data::String(" RIGHT".into()));
    });
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams_pick()).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(get_h(&m, ids[0]).unwrap(), 2.0);
    assert_eq!(get_h(&m, ids[1]).unwrap(), 2.0);
    let bad = edited(&p, "fam_pick_bad", |g| {
        set(g, 1, PICK_HEAD, Data::String("Middle".into()));
    });
    let rep = import_xlsx_families(&mut m, &bad, opts(true), &fams_pick()).unwrap();
    assert_eq!(rep.rejected.len(), 1, "{rep:?}");
    assert!(rep.rejected[0].reason.contains("unknown choice"), "{:?}", rep.rejected);
}

#[test]
fn legacy_numeric_type_id_still_accepted() {
    let (mut m, ids, rib) = model_pick();
    let p = pick_book(&m, "fam_legacy_src");
    let q = edited(&p, "fam_legacy", |g| {
        set(g, 1, TYPE_HEAD, Data::Float(rib as f64));
        set(g, 1, PICK_HEAD, Data::Float(2.0));
        set(g, 2, TYPE_HEAD, Data::String(rib.to_string()));
    });
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams_pick()).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(get_t(&m, ids[0]).unwrap(), rib as f64);
    assert_eq!(get_t(&m, ids[1]).unwrap(), rib as f64);
    assert_eq!(get_h(&m, ids[0]).unwrap(), 2.0);
}

#[test]
fn ambiguous_type_name_refused() {
    let (mut m, ids, _) = model_pick();
    let layer = || LayerSpec {
        material: "plaster".into(),
        thickness: 0.02,
    };
    let a = m.add_slab_type("Dup", &[layer()]).unwrap();
    let b = m.add_slab_type("dup ", &[layer()]).unwrap();
    let p = pick_book(&m, "fam_amb_src");
    let before = get_t(&m, ids[0]).unwrap();
    let q = edited(&p, "fam_amb", |g| {
        set(g, 1, TYPE_HEAD, Data::String("DUP".into()));
    });
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams_pick()).unwrap();
    assert_eq!(rep.rejected.len(), 1, "{rep:?}");
    let why = &rep.rejected[0].reason;
    assert!(why.contains("ambiguous") && why.contains(&a.to_string()) && why.contains(&b.to_string()), "{why}");
    assert_eq!(get_t(&m, ids[0]).unwrap(), before);
    // the number still names one of them
    let q = edited(&p, "fam_amb_id", |g| {
        set(g, 1, TYPE_HEAD, Data::Float(b as f64));
    });
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams_pick()).unwrap();
    assert!(rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(get_t(&m, ids[0]).unwrap(), b as f64);
}

/// Pick of the first row edited, the type cell left as exported.
fn pick_edit(p: &PathBuf, tag: &str) -> PathBuf {
    edited(p, tag, |g| set(g, 1, PICK_HEAD, Data::String("Right".into())))
}

#[test]
fn family_untouched_cell_with_renamed_type_is_not_rejected() {
    let (mut m, ids, _) = model_pick();
    let before = get_t(&m, ids[0]).unwrap();
    let p = pick_book(&m, "fam_ren_src");
    m.sql().execute("UPDATE slab_type SET name='Roof deck' WHERE id=?1", [before as i64]).unwrap();
    let q = pick_edit(&p, "fam_ren");
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams_pick()).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(get_h(&m, ids[0]).unwrap(), 2.0);
    assert_eq!(get_t(&m, ids[0]).unwrap(), before);
}

#[test]
fn family_untouched_cell_with_deleted_type_is_not_rejected() {
    let (mut m, ids, rib) = model_pick();
    let before = get_t(&m, ids[0]).unwrap();
    set_t(&mut m, ids[0], rib as f64).unwrap();
    set_t(&mut m, ids[1], rib as f64).unwrap();
    let p = pick_book(&m, "fam_del_src");
    set_t(&mut m, ids[0], before).unwrap();
    set_t(&mut m, ids[1], before).unwrap();
    m.sql().execute("DELETE FROM slab_type_layer WHERE stype=?1", [rib]).unwrap();
    m.sql().execute("DELETE FROM slab_type WHERE id=?1", [rib]).unwrap();
    let q = pick_edit(&p, "fam_del");
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams_pick()).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(get_h(&m, ids[0]).unwrap(), 2.0);
    assert_eq!(get_t(&m, ids[0]).unwrap(), before);
}

#[test]
fn family_untouched_cell_with_newly_duplicate_type_name_is_not_ambiguous() {
    let (mut m, ids, _) = model_pick();
    let before = get_t(&m, ids[0]).unwrap();
    let p = pick_book(&m, "fam_dup_src");
    let layer = LayerSpec {
        material: "plaster".into(),
        thickness: 0.02,
    };
    m.add_slab_type("CEILING ", &[layer]).unwrap();
    let q = pick_edit(&p, "fam_dup");
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams_pick()).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(get_h(&m, ids[0]).unwrap(), 2.0);
    assert_eq!(get_t(&m, ids[0]).unwrap(), before);
    // a name the user types is still checked
    let layer = LayerSpec {
        material: "plaster".into(),
        thickness: 0.02,
    };
    m.add_slab_type("rib", &[layer]).unwrap();
    let bad = edited(&p, "fam_dup_typed", |g| set(g, 1, TYPE_HEAD, Data::String("RIB".into())));
    let rep = import_xlsx_families(&mut m, &bad, opts(true), &fams_pick()).unwrap();
    assert_eq!(rep.rejected.len(), 1, "{rep:?}");
    assert!(rep.rejected[0].reason.contains("ambiguous"), "{rep:?}");
}

fn bad_id_pick_book(tag: &str) -> (Model, Vec<i64>, PathBuf) {
    let (m, ids, _) = model_pick();
    let p = pick_book(&m, &format!("{tag}_src"));
    let q = edited(&p, tag, |g| {
        set(g, 1, PICK_HEAD, Data::String("Right".into()));
        set(g, 2, "id", Data::String("7x".into()));
    });
    (m, ids, q)
}

fn ask_deletes() -> ImportOptions {
    ImportOptions { apply: true, delete_missing: true, new_params: false }
}

#[test]
fn family_bad_id_cell_keeps_other_rows_and_skips_deletes() {
    let (mut m, ids, q) = bad_id_pick_book("fam_bid_keep");
    let rep = import_xlsx_families(&mut m, &q, ask_deletes(), &fams_pick()).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(get_h(&m, ids[0]).unwrap(), 2.0);
    assert_eq!(ceilings(&m), 2);
    assert!(rep.changes.iter().all(|c| c.kind() != "deleted"), "{rep:?}");
    assert!(rep.rejected.iter().all(|r| r.row == 3), "{:?}", rep.rejected);
}

#[test]
fn family_bad_id_does_not_block_book() {
    let (mut m, _, q) = bad_id_pick_book("fam_bid_block");
    let rep = import_xlsx_families(&mut m, &q, ask_deletes(), &fams_pick()).unwrap();
    assert!(rep.applied && rep.blocking() == 0 && !rep.rejected.is_empty(), "{rep:?}");
}

#[test]
fn family_bad_id_message_matches_main_import() {
    let (mut m, _, q) = bad_id_pick_book("fam_bid_msg");
    let rep = import_xlsx_families(&mut m, &q, ask_deletes(), &fams_pick()).unwrap();
    let why = rep
        .rejected
        .iter()
        .find(|r| r.reason == super::import::NO_DELETES)
        .unwrap_or_else(|| panic!("{:?}", rep.rejected));
    assert!(why.row == 3 && why.column == "id", "{why:?}");
    assert!(rep.rejected.iter().any(|r| r.row == why.row && r.reason != super::import::NO_DELETES));
}

fn pin(m: &mut Model, id: i64) -> String {
    assert!(m.can_pin("ceiling"));
    assert_eq!(m.pin(&[("ceiling", id)]).unwrap(), 1);
    format!("ceiling {id} is pinned: skipped")
}

#[test]
fn family_row_removed_keeps_pinned_element() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_pin_miss_src", Units::Metric);
    let why = pin(&mut m, ids[1]);
    let q = edited(&p, "fam_pin_miss", |g| {
        g.remove(2);
    });
    let o = ImportOptions { apply: true, delete_missing: true, new_params: false };
    let rep = import_xlsx_families(&mut m, &q, o, &fams()).unwrap();
    assert_eq!(rep.counts().deleted, 0, "{rep:?}");
    assert!(rep.ignored.iter().any(|i| i.id == Some(ids[1]) && i.reason == why), "{rep:?}");
    assert!(rep.removed.iter().all(|r| r.id != ids[1]), "{rep:?}");
    assert_eq!(ceilings(&m), 2);
}

#[test]
fn family_delete_flag_refused_for_pinned() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_pin_del_src", Units::Metric);
    let why = pin(&mut m, ids[1]);
    let q = edited(&p, "fam_pin_del", |g| set(g, 2, "delete", Data::String("delete".into())));
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(rep.props.is_empty() && rep.counts().deleted == 0, "{rep:?}");
    assert!(rep.ignored.iter().any(|i| i.row == 3 && i.reason == why), "{rep:?}");
    assert_eq!(ceilings(&m), 2);
}

#[test]
fn family_edit_refused_for_pinned_and_backstop_filters_props() {
    let (mut m, ids) = model();
    let p = book(&m, "fam_pin_edit_src", Units::Metric);
    let q = edited(&p, "fam_pin_edit", |g| {
        set(g, 1, "Height [m]", Data::Float(3.3));
        set(g, 2, "Height [m]", Data::Float(3.1));
    });
    // props read before the pin (a stale preview) are still filtered on apply
    let stale = import_xlsx_families(&mut m, &q, opts(false), &fams()).unwrap().props;
    assert_eq!(stale.len(), 2);
    let why = pin(&mut m, ids[1]);
    let rep = import_xlsx_families(&mut m, &q, opts(true), &fams()).unwrap();
    assert!(rep.ignored.iter().any(|i| i.row == 3 && i.reason == why), "{rep:?}");
    assert!(rep.changes.iter().all(|c| c.id != Some(ids[1])), "{rep:?}");
    assert!((get_h(&m, ids[1]).unwrap() - 2.6).abs() < 1e-12);
    assert!((get_h(&m, ids[0]).unwrap() - 3.3).abs() < 1e-12);
    let (keep, skipped) = split_pinned_props(&m, &stale).unwrap();
    assert_eq!(skipped, vec![("ceiling".to_string(), ids[1])]);
    assert!(keep.iter().all(|k| k.id != ids[1]) && keep.len() == 1);
}
