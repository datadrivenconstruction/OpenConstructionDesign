//! An Apply that does not rewrite the whole book: a value put back in the book afterwards is
//! one change, never "model newer"; an untouched book is no change; undo and redo hold.
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_io::xlsx;

use super::super::xlsx_job::Outcome;
use super::super::xlsx_win::{apply_begin, apply_finish, XlsxPreview};
use crate::editor::Editor;
use crate::xl_text::Lang;

type Grid = Vec<Vec<Data>>;

const MINE: &str = "My walls";
const H: &str = "height [m]";

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let lv = ed.level;
    let t = ed
        .model
        .add_wall_type("ext", &[ddc_model::LayerSpec { material: "brick".into(), thickness: 0.25 }])
        .unwrap();
    for (a, b) in [((0., 0.), (6., 0.)), ((6., 0.), (6., 4.))] {
        ed.model
            .add_wall(&ddc_model::WallSpec { a, b, level: lv, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None })
            .unwrap();
    }
    ed.reload().unwrap();
    ed
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc_xs49_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn grids(p: &Path) -> Vec<(String, Grid)> {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.sheet_names()
        .into_iter()
        .map(|n| {
            let g: Grid = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
            (n, g)
        })
        .collect()
}

fn save(dst: &Path, sheets: &[(String, Grid)]) {
    let mut out = rust_xlsxwriter::Workbook::new();
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

/// The book saved again with the first wall's height cell of `sheet` set to `h`.
fn put_height(p: &Path, sheet: &str, h: f64) {
    let mut sheets = grids(p);
    let g = &mut sheets.iter_mut().find(|s| s.0 == sheet).unwrap().1;
    let c = g[0].iter().position(|d| d.to_string() == H).unwrap();
    g[1][c] = Data::Float(h);
    save(p, &sheets);
}

/// The exported book with the first wall 4 m high; the Walls sheet renamed `MINE` when `alias`.
fn book(ed: &Editor, tag: &str, alias: bool) -> PathBuf {
    let src = dir(tag).join("plan.xlsx");
    xlsx::export_xlsx(&ed.model, &src).unwrap();
    put_height(&src, "Walls", 4.0);
    if alias {
        let mut sheets = grids(&src);
        sheets.iter_mut().find(|s| s.0 == "Walls").unwrap().0 = MINE.into();
        save(&src, &sheets);
    }
    src
}

fn preview(ed: &mut Editor, p: &Path, alias: bool) -> XlsxPreview {
    let mut pv = XlsxPreview::load(&mut ed.model, p, false).unwrap();
    if alias {
        pv.read_as(&mut ed.model, "Walls", Some(MINE));
    }
    pv.rev = ed.content_snap();
    super::super::xlsx_no_base::confirm(&mut pv);
    pv
}

fn reread(ed: &mut Editor, pv: &mut XlsxPreview) {
    pv.reload(&mut ed.model);
    pv.rev = ed.content_snap();
    super::super::xlsx_no_base::confirm(pv);
}

fn first_height(ed: &Editor) -> f64 {
    ed.model.sql().query_row("SELECT height FROM wall ORDER BY id LIMIT 1", [], |r| r.get(0)).unwrap()
}

/// Apply as the window does it; the re-export ends with `o` (no file is written here).
fn apply(ed: &mut Editor, pv: &mut XlsxPreview, o: Outcome) -> super::super::xlsx_job::ApplyTail {
    let opts = apply_begin(ed, pv, Lang::En).unwrap().unwrap();
    let tail = opts.apply.clone().unwrap();
    apply_finish(ed.content_snap(), &mut ed.model, pv, &tail, &o);
    tail
}

/// One change, the height cell, offered as a book edit.
fn one_height_change(pv: &XlsxPreview) {
    let r = &pv.report;
    assert!(!r.no_base, "the book keeps its baseline");
    assert_eq!((r.changes.len(), r.model_newer, r.conflicts.len()), (1, 0, 0), "{:?}", r.changes);
    assert!(r.changes[0].column.contains("height"), "{:?}", r.changes[0]);
}

#[test]
fn aliased_sheet_revert_after_apply_is_one_change() {
    let mut ed = editor();
    let p = book(&ed, "alias", true);
    let mut pv = preview(&mut ed, &p, true);
    let bytes = std::fs::read(&p).unwrap();
    let tail = apply(&mut ed, &mut pv, Outcome::Cancelled);
    assert!(tail.skip && tail.ids_in_place, "{tail:?}");
    assert_eq!(first_height(&ed), 4.0);
    assert_eq!(std::fs::read(&p).unwrap(), bytes, "the user's book is not rewritten");
    // the user puts the exported value back in the renamed sheet
    put_height(&p, MINE, 3.0);
    reread(&mut ed, &mut pv);
    one_height_change(&pv);
    let u0 = ed.undo_len();
    apply(&mut ed, &mut pv, Outcome::Cancelled);
    assert_eq!(first_height(&ed), 3.0, "the revert reaches the model");
    assert_eq!(ed.undo_len(), u0 + 1, "one Apply, one undo step");
}

#[test]
fn failed_reexport_revert_after_apply_is_one_change() {
    let mut ed = editor();
    let p = book(&ed, "failed", false);
    let mut pv = preview(&mut ed, &p, false);
    let bytes = std::fs::read(&p).unwrap();
    let tail = apply(&mut ed, &mut pv, Outcome::Failed("locked".into()));
    assert!(!tail.skip);
    assert!(!pv.synced, "the book was not written");
    assert_eq!(std::fs::read(&p).unwrap(), bytes);
    put_height(&p, "Walls", 3.0);
    reread(&mut ed, &mut pv);
    one_height_change(&pv);
}

#[test]
fn untouched_book_after_apply_is_no_change() {
    let mut ed = editor();
    let p = book(&ed, "same", true);
    let mut pv = preview(&mut ed, &p, true);
    apply(&mut ed, &mut pv, Outcome::Cancelled);
    reread(&mut ed, &mut pv);
    let r = &pv.report;
    assert!(r.changes.is_empty() && r.conflicts.is_empty(), "{:?}", r.changes);
    let (u0, top) = (ed.undo_len(), ed.model.op_top().ok());
    let _ = apply_begin(&mut ed, &mut pv, Lang::En);
    assert_eq!((ed.undo_len(), ed.model.op_top().ok()), (u0, top), "a true no-op writes nothing");
    assert_eq!(first_height(&ed), 4.0);
}

#[test]
fn undo_and_redo_of_apply_keep_the_compare_right() {
    let mut ed = editor();
    let p = book(&ed, "undo", true);
    let mut pv = preview(&mut ed, &p, true);
    apply(&mut ed, &mut pv, Outcome::Cancelled);
    ed.undo();
    assert_eq!(first_height(&ed), 3.0, "undo restores the model");
    // the undone book value is offered again, not taken for a newer model value
    reread(&mut ed, &mut pv);
    one_height_change(&pv);
    ed.redo();
    assert_eq!(first_height(&ed), 4.0, "redo repeats the Apply");
    put_height(&p, MINE, 3.0);
    reread(&mut ed, &mut pv);
    one_height_change(&pv);
}

#[test]
fn model_edit_after_apply_is_never_overwritten() {
    let mut ed = editor();
    let p = book(&ed, "medit", true);
    let mut pv = preview(&mut ed, &p, true);
    apply(&mut ed, &mut pv, Outcome::Cancelled);
    let id: i64 = ed.model.sql().query_row("SELECT min(id) FROM wall", [], |r| r.get(0)).unwrap();
    ed.model.sql().execute("UPDATE wall SET height=3.5 WHERE id=?1", [id]).unwrap();
    // the book still shows the applied value: the model moved on since, nothing to take
    reread(&mut ed, &mut pv);
    assert!(pv.report.changes.is_empty(), "{:?}", pv.report.changes);
    put_height(&p, MINE, 3.0);
    reread(&mut ed, &mut pv);
    assert!(pv.report.changes.is_empty(), "{:?}", pv.report.changes);
    assert_eq!(first_height(&ed), 3.5, "the model edit stays");
}

#[test]
fn fresh_reexport_drops_the_applied_base() {
    let mut ed = editor();
    let p = book(&ed, "fresh", false);
    let mut pv = preview(&mut ed, &p, false);
    let opts = apply_begin(&mut ed, &mut pv, Lang::En).unwrap().unwrap();
    assert!(!super::get(&ed.model, &p).is_empty(), "the applied cell is remembered until the book is written");
    xlsx::export_xlsx(&ed.model, &p).unwrap();
    let tail = opts.apply.as_ref().unwrap();
    apply_finish(ed.content_snap(), &mut ed.model, &mut pv, tail, &Outcome::Done);
    assert!(super::get(&ed.model, &p).is_empty(), "the fresh baseline is the base again");
    reread(&mut ed, &mut pv);
    assert!(pv.report.changes.is_empty(), "{:?}", pv.report.changes);
}

fn uid(ed: &Editor) -> Option<i64> {
    super::super::xlsx_project::model_uid(&ed.model)
}

/// `ed` with one cell applied from its own book (re-export cancelled: the base stays here).
fn applied(ed: &mut Editor, tag: &str) -> PathBuf {
    let p = book(ed, tag, false);
    let mut pv = preview(ed, &p, false);
    apply(ed, &mut pv, Outcome::Cancelled);
    assert!(!super::get(&ed.model, &p).is_empty());
    p
}

#[test]
fn two_projects_with_one_book_path_stay_apart() {
    let mut a = editor();
    let b = editor();
    assert_ne!(uid(&a), uid(&b), "two projects, two uids");
    let p = applied(&mut a, "two");
    assert!(super::get(&b.model, &p).is_empty(), "the other project never sees this base");
    // the other project's re-export clears only its own entry
    super::clear(&b.model, &p);
    assert!(!super::get(&a.model, &p).is_empty());
    super::clear(&a.model, &p);
    assert!(super::get(&a.model, &p).is_empty());
}

#[test]
fn reopening_the_project_keeps_its_base() {
    let mut ed = editor();
    let d = dir("reopen");
    let proj = d.join("a.ddc");
    ed.save_project_as(&proj).unwrap();
    let p = applied(&mut ed, "reopen_book");
    let before = uid(&ed);
    ed.open_project(&proj).unwrap();
    assert_eq!(uid(&ed), before, "the same project came back");
    assert!(!super::get(&ed.model, &p).is_empty(), "the base stays for this session");
}

#[test]
fn new_project_drops_the_closed_projects_base() {
    let mut ed = editor();
    let p = applied(&mut ed, "newp");
    let old = uid(&ed);
    ed.new_project().unwrap();
    assert_ne!(uid(&ed), old);
    assert!(super::get(&ed.model, &p).is_empty());
    assert!(super::get_in(old, &p).is_empty(), "the closed project's entry is gone");
}

#[test]
fn opening_another_project_drops_the_closed_projects_base() {
    let d = dir("openother");
    let other = d.join("b.ddc");
    let mut b = editor();
    b.save_project_as(&other).unwrap();
    let ub = uid(&b);
    drop(b);
    let mut ed = editor();
    let p = applied(&mut ed, "openother_book");
    let old = uid(&ed);
    ed.open_project(&other).unwrap();
    assert_eq!(uid(&ed), ub);
    assert!(super::get(&ed.model, &p).is_empty());
    assert!(super::get_in(old, &p).is_empty(), "the closed project's entry is gone");
}

#[test]
fn replacing_a_model_keeps_other_projects_entries() {
    let mut a = editor();
    let mut b = editor();
    let pa = applied(&mut a, "keep_a");
    let pb = applied(&mut b, "keep_b");
    a.new_project().unwrap();
    assert!(!super::get(&b.model, &pb).is_empty(), "b's base is not a's to drop");
    assert!(super::get(&a.model, &pa).is_empty());
}
