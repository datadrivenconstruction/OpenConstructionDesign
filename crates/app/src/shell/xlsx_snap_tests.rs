//! The Apply gate keys a preview on the model's content, not on the revision: an equal revision
//! over other content never lets a stale report write, and a reload never makes one stale.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ddc_io::xlsx;

use super::super::xlsx_job::{ApplyTail, Outcome};
use super::super::xlsx_load::XlsxLoad;
use super::{
    apply_begin, apply_finish, apply_preview, check_slot, collapsed_reason, demo_book, Applied,
    XlsxPreview, Reloaded,
};
use crate::editor::{Editor, OpFn};
use crate::xl_text::Lang;

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let lv = ed.level;
    let t = ed
        .model
        .add_wall_type("ext", &[ddc_model::LayerSpec { material: "brick".into(), thickness: 0.25 }])
        .unwrap();
    let p = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];
    for k in 0..4 {
        ed.model
            .add_wall(&ddc_model::WallSpec {
                a: p[k],
                b: p[(k + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
    }
    ed.reload().unwrap();
    ed
}

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc_app_r93d_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `ed`'s export with its first wall set to 4.0 in the book, as a user edits it.
fn edited_book(ed: &Editor, d: &Path) -> PathBuf {
    let src = d.join("src.xlsx");
    let fams = crate::elem::sheets::families();
    xlsx::export_xlsx_families(&ed.model, &src, xlsx::Units::Metric, &fams).unwrap();
    let dst = d.join("edited.xlsx");
    let first = ed.model.sql().query_row("SELECT min(id) FROM wall", [], |r| r.get(0)).unwrap();
    xlsx::patch_cell(&src, &dst, "Walls", first, "height [m]", 4.0).unwrap();
    dst
}

fn preview(ed: &mut Editor, book: &Path) -> XlsxPreview {
    let mut pv = XlsxPreview::load(&mut ed.model, book, false).unwrap();
    pv.rev = ed.content_snap();
    assert_eq!(pv.report.counts().changed, 1);
    pv
}

fn heights(ed: &Editor) -> Vec<f64> {
    let mut st = ed.model.sql().prepare("SELECT height FROM wall ORDER BY id").unwrap();
    st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
}

fn add_level(ed: &mut Editor, name: &'static str) {
    ed.run_op(OpFn(Arc::new(move |m: &mut ddc_model::Model| m.add_level(name, 6.0).map(|_| ()))))
        .unwrap();
}

fn apply(ed: &mut Editor, pv: &mut XlsxPreview) -> Result<Applied, String> {
    apply_preview(ed, pv, xlsx::Units::Metric, Lang::En)
}

/// The gate refused without writing a thing; the fresh report then applies the book's value.
fn stale_then_done(ed: &mut Editor, pv: &mut XlsxPreview) {
    let before = heights(ed);
    let top = ed.model.op_top().unwrap();
    assert_eq!(apply(ed, pv), Ok(Applied::Stale), "the report came from another model state");
    assert_eq!(heights(ed), before);
    assert_eq!(ed.model.op_top().unwrap(), top);
    assert_eq!(pv.rev, ed.content_snap(), "read again: the next Apply passes");
    assert!(matches!(apply(ed, pv), Ok(Applied::Done { .. })));
    assert!((heights(ed)[0] - 4.0).abs() < 1e-9);
}

#[test]
fn normal_apply_writes_the_book_value() {
    let d = dir("normal");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    let mut pv = preview(&mut ed, &book);
    assert!(matches!(apply(&mut ed, &mut pv), Ok(Applied::Done { .. })));
    assert!((heights(&ed)[0] - 4.0).abs() < 1e-9);
}

#[test]
fn reload_and_settings_do_not_make_a_preview_stale() {
    let d = dir("reload");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    let mut pv = preview(&mut ed, &book);
    let rev = ed.revision;
    ed.reload().unwrap();
    assert!(ed.revision > rev, "the view reloaded, the content did not move");
    ed.model
        .sql()
        .execute("INSERT OR REPLACE INTO meta(key,value) VALUES('pref:r93d','1')", [])
        .unwrap();
    assert_eq!(pv.rev, ed.content_snap());
    assert!(matches!(apply(&mut ed, &mut pv), Ok(Applied::Done { .. })));
    assert!((heights(&ed)[0] - 4.0).abs() < 1e-9);
}

#[test]
fn undo_then_apply_is_stale() {
    let d = dir("undo");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    add_level(&mut ed, "Mark");
    let mut pv = preview(&mut ed, &book);
    ed.undo();
    stale_then_done(&mut ed, &mut pv);
}

#[test]
fn edit_then_undo_back_to_the_preview_reads_again() {
    let d = dir("undo_back");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    let mut pv = preview(&mut ed, &book);
    add_level(&mut ed, "Mark");
    ed.undo();
    stale_then_done(&mut ed, &mut pv);
}

const FILTER: &str = "walls height>3";

/// An undoable step that writes only settings (`meta`): the saved filter list.
fn save_filter(ed: &mut Editor) {
    ed.run_op(OpFn(Arc::new(|m: &mut ddc_model::Model| {
        m.set_saved_filters(&[FILTER.to_string()]).map(|_| ())
    })))
    .unwrap();
}

#[test]
fn meta_only_step_then_apply_is_stale() {
    let d = dir("meta_step");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    let mut pv = preview(&mut ed, &book);
    save_filter(&mut ed);
    stale_then_done(&mut ed, &mut pv);
}

/// Undo of a meta-only step brings its old mark back, and with it the very state the preview
/// was read on: passing is right there.
#[test]
fn meta_only_step_undone_back_to_the_preview_applies() {
    let d = dir("meta_undo_back");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    let mut pv = preview(&mut ed, &book);
    let filters = ed.model.saved_filters().unwrap();
    save_filter(&mut ed);
    assert_ne!(pv.rev, ed.content_snap());
    ed.undo();
    assert_eq!(ed.model.saved_filters().unwrap(), filters, "the settings came back too");
    assert_eq!(pv.rev, ed.content_snap(), "the same state, the same mark");
    assert!(matches!(apply(&mut ed, &mut pv), Ok(Applied::Done { .. })));
    assert!((heights(&ed)[0] - 4.0).abs() < 1e-9);
}

#[test]
fn meta_only_step_undone_after_the_preview_is_stale() {
    let d = dir("meta_undo");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    save_filter(&mut ed);
    let mut pv = preview(&mut ed, &book);
    ed.undo();
    stale_then_done(&mut ed, &mut pv);
}

#[test]
fn meta_only_step_redone_past_the_preview_is_stale() {
    let d = dir("meta_redo");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    let mut pv = preview(&mut ed, &book);
    save_filter(&mut ed);
    ed.undo();
    ed.redo();
    stale_then_done(&mut ed, &mut pv);
}

/// Redo brings back the settings the preview was read on, yet a new stamp: a false stale,
/// never a stale pass.
#[test]
fn meta_only_step_undone_and_redone_to_the_preview_is_stale() {
    let d = dir("meta_undo_redo");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    save_filter(&mut ed);
    let mut pv = preview(&mut ed, &book);
    ed.undo();
    ed.redo();
    assert_eq!(ed.model.saved_filters().unwrap(), Some(vec![FILTER.to_string()]));
    stale_then_done(&mut ed, &mut pv);
}

/// The hole of the bare revision: `Editor::undo` leaves this state when its reload fails, the
/// step is taken back and no reload path moved the revision.
#[test]
fn undo_whose_reload_failed_keeps_the_revision_and_is_still_stale() {
    let d = dir("undo_no_reload");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    add_level(&mut ed, "Mark");
    let mut pv = preview(&mut ed, &book);
    let rev = ed.revision;
    ed.model.undo().unwrap();
    assert_eq!(ed.revision, rev, "equal revision, other content");
    stale_then_done(&mut ed, &mut pv);
}

/// A read that failed leaves the old report on screen and its stamp marked read: it stays stale.
#[test]
fn failed_read_never_lets_the_old_report_through() {
    let d = dir("bad_read");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    let good = std::fs::read(&book).unwrap();
    let mut pv = preview(&mut ed, &book);
    add_level(&mut ed, "Mark");
    std::fs::write(&book, b"not a workbook").unwrap();
    let before = heights(&ed);
    let top = ed.model.op_top().unwrap();
    for _ in 0..2 {
        assert_eq!(apply(&mut ed, &mut pv), Ok(Applied::Stale));
        assert!(pv.error.is_some());
        assert_eq!(pv.rev, None);
    }
    assert_eq!(heights(&ed), before);
    assert_eq!(ed.model.op_top().unwrap(), top);
    std::fs::write(&book, good).unwrap();
    assert_eq!(apply(&mut ed, &mut pv), Ok(Applied::Stale), "the book is read first");
    assert!(pv.error.is_none());
    assert_eq!(pv.rev, ed.content_snap());
    assert!(matches!(apply(&mut ed, &mut pv), Ok(Applied::Done { .. })));
    assert!((heights(&ed)[0] - 4.0).abs() < 1e-9);
}

/// A choice to keep the disclosure closed never reaches a later report with blocking items.
#[test]
fn blocking_items_open_the_disclosure_for_each_report() {
    let (a, b) = (Path::new("a.xlsx"), Path::new("b.xlsx"));
    let (quiet, closed) = check_slot(a, 1, false);
    assert!(!closed);
    assert_eq!(quiet, check_slot(b, 2, false).0, "one choice for quiet items");
    let (block, open) = check_slot(a, 1, true);
    assert!(open);
    assert_ne!(block, quiet);
    assert_ne!(block, check_slot(a, 2, true).0, "a new report asks again");
    assert_ne!(block, check_slot(b, 1, true).0, "another book asks again");
}

#[test]
fn closed_disclosure_still_names_what_blocks_apply() {
    assert_eq!(collapsed_reason(true, false, Some("why")), Some("why"));
    assert_eq!(collapsed_reason(true, true, Some("why")), None, "the items are on screen");
    assert_eq!(collapsed_reason(false, false, Some("why")), None, "the footer says it");
    assert_eq!(collapsed_reason(true, false, None), None, "ticked: nothing blocks");
}

#[test]
fn restore_revision_then_apply_is_stale() {
    let d = dir("restore");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    let mut pv = preview(&mut ed, &book);
    let rev = ed.revision;
    add_level(&mut ed, "Mark");
    ed.restore_revision(rev);
    assert_eq!(ed.revision, rev, "equal revision, other content");
    stale_then_done(&mut ed, &mut pv);
}

#[test]
fn level_walk_alone_does_not_make_a_preview_stale() {
    let d = dir("walk");
    let mut ed = editor();
    add_level(&mut ed, "Upper");
    let book = edited_book(&ed, &d);
    let mut pv = preview(&mut ed, &book);
    let _ = ed.scene_all();
    assert_eq!(pv.rev, ed.content_snap());
    assert!(matches!(apply(&mut ed, &mut pv), Ok(Applied::Done { .. })));
}

#[test]
fn swap_model_then_apply_never_writes_the_old_report() {
    let d = dir("swap");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    let mut pv = preview(&mut ed, &book);
    let rev = ed.revision;
    // same ids and same heights, another project; the revision is even forced back
    ed.swap_model(editor().model, None).unwrap();
    ed.restore_revision(rev);
    assert_ne!(pv.rev, ed.content_snap());
    let h0 = heights(&ed);
    assert_eq!(apply(&mut ed, &mut pv), Ok(Applied::Stale));
    assert_eq!(heights(&ed), h0, "the other project's walls stay");
    assert!(pv.report.changes.is_empty(), "the old report is gone");
}

#[test]
fn worker_read_finishing_after_the_model_changed_is_stale() {
    let d = dir("worker_read");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    let mut pv = XlsxPreview::linked(&book, false);
    let job = XlsxLoad::start(&ed.model, &book, false, ed.content_snap()).unwrap();
    add_level(&mut ed, "Mark");
    let out = loop {
        if let Some(o) = job.poll() {
            break o;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(pv.finish_reload(out, job.rev), Reloaded::Applied);
    assert_ne!(pv.rev, ed.content_snap());
    stale_then_done(&mut ed, &mut pv);
}

#[test]
fn re_export_finishing_after_the_model_changed_stays_stale() {
    let d = dir("worker_apply");
    let mut ed = editor();
    let book = edited_book(&ed, &d);
    let mut pv = preview(&mut ed, &book);
    let opts = apply_begin(&mut ed, &mut pv, Lang::En).unwrap().expect("not stale");
    let tail = opts.apply.clone().unwrap();
    let started = ed.content_snap();
    add_level(&mut ed, "During the job");
    apply_finish(started, &mut ed.model, &mut pv, &tail, &Outcome::Done);
    assert_eq!(pv.rev, started, "no false fresh: the next Apply reads again");
    assert_ne!(pv.rev, ed.content_snap());
}

/// `apply_finish` clears the created-row records itself: its own write is no stale.
#[test]
fn own_bookkeeping_after_the_re_export_is_not_stale() {
    let d = dir("own");
    let mut ed = editor();
    let book = d.join("demo.xlsx");
    demo_book(&ed.model, &book).unwrap();
    let mut pv = XlsxPreview::load(&mut ed.model, &book, false).unwrap();
    pv.rev = ed.content_snap();
    crate::shell::xlsx_no_base::confirm(&mut pv);
    let opts = apply_begin(&mut ed, &mut pv, Lang::En).unwrap().expect("not stale");
    let tail: ApplyTail = opts.apply.clone().unwrap();
    assert!(tail.created > 0);
    let started = ed.content_snap();
    apply_finish(started, &mut ed.model, &mut pv, &tail, &Outcome::Done);
    assert_ne!(ed.content_snap(), started, "the records were cleared");
    assert_eq!(pv.rev, ed.content_snap());
}
