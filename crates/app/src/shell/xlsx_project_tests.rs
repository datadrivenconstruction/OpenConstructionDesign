//! A book of another project never reaches this one; the link follows the open project.
use super::*;
use crate::editor::Editor;
use crate::shell::xlsx_job::{sync_action, SyncAction};
use crate::shell::xlsx_win::{apply_preview, book_error, last_book, Applied, XlsxPreview};
use crate::shell::EditorApp;
use crate::xl_text::Lang;
use std::path::PathBuf;

/// Four walls, ids 1..4 in every fresh project: the ids of two projects collide.
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
    let d = std::env::temp_dir().join(format!("ddc_app_xs21_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn first_wall(ed: &Editor) -> i64 {
    ed.model.sql().query_row("SELECT min(id) FROM wall", [], |r| r.get(0)).unwrap()
}

fn heights(ed: &Editor) -> Vec<f64> {
    let mut st = ed.model.sql().prepare("SELECT height FROM wall ORDER BY id").unwrap();
    st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
}

/// `ed`'s export with its first wall set to 4.0 in the book, as a user edits it.
fn edited_book(ed: &Editor, d: &std::path::Path, name: &str) -> PathBuf {
    let src = d.join(format!("{name}-src.xlsx"));
    xlsx::export_xlsx_families(&ed.model, &src, xlsx::Units::Metric, &crate::elem::sheets::families()).unwrap();
    let dst = d.join(format!("{name}.xlsx"));
    xlsx::patch_cell(&src, &dst, "Walls", first_wall(ed), "height [m]", 4.0).unwrap();
    dst
}

fn app(ed: Editor) -> EditorApp {
    EditorApp::new(ed, None, "test".into())
}

#[test]
fn book_of_another_project_is_refused_before_the_read() {
    let d = dir("other");
    let a = editor();
    let book = edited_book(&a, &d, "a");
    let mut b = editor();
    assert_ne!(model_uid(&a.model), model_uid(&b.model));
    assert_eq!(origin(&b.model, &book), Origin::Other);
    let got = XlsxPreview::load(&mut b.model, &book, false);
    assert_eq!(got.err().as_deref(), Some(FOREIGN), "no report is computed against project B");
    // the same book in its own project reads as before
    let mut a = a;
    assert_eq!(origin(&a.model, &book), Origin::Same);
    let pv = XlsxPreview::load(&mut a.model, &book, false).unwrap();
    assert_eq!(pv.report.counts().changed, 1);
    assert!(!pv.report.no_project);
}

#[test]
fn refused_book_opens_the_card_and_apply_changes_nothing() {
    let d = dir("card");
    let book = edited_book(&editor(), &d, "a");
    let mut b = app(editor());
    b.xlsx_open_now(&book, false);
    let pv = b.xlsx.as_mut().expect("the card shows the refused book");
    assert!(is_foreign(pv));
    assert!(pv.report.changes.is_empty() && pv.rev.is_none() && !pv.synced);
    assert_eq!(last_book(), None, "the link never points at another project's book");
    let (rev, h0, hash) = (b.ed.revision, heights(&b.ed), b.ed.model.content_hash().unwrap());
    let mut pv = b.xlsx.take().unwrap();
    let r = apply_preview(&mut b.ed, &mut pv, xlsx::Units::Metric, Lang::En);
    assert_eq!(r, Err(FOREIGN.to_string()));
    assert_eq!(b.ed.revision, rev);
    assert_eq!(heights(&b.ed), h0);
    assert_eq!(b.ed.model.content_hash().unwrap(), hash);
}

#[test]
fn linked_book_replaced_by_another_projects_book_drops_the_report() {
    let d = dir("swap_file");
    let foreign = edited_book(&editor(), &d, "a");
    let mut b = editor();
    let own = edited_book(&b, &d, "b");
    let mut pv = XlsxPreview::load(&mut b.model, &own, false).unwrap();
    pv.rev = b.content_snap();
    assert_eq!(pv.report.counts().changed, 1);
    std::fs::copy(&foreign, &own).unwrap();
    pv.reload(&mut b.model);
    assert!(is_foreign(&pv));
    assert!(pv.report.changes.is_empty(), "the old report cannot be applied either");
    let h0 = heights(&b);
    let r = apply_preview(&mut b, &mut pv, xlsx::Units::Metric, Lang::En);
    assert_eq!(r, Err(FOREIGN.to_string()));
    assert_eq!(heights(&b), h0);
}

#[test]
fn same_project_book_applies() {
    let d = dir("same");
    let mut ed = editor();
    let book = edited_book(&ed, &d, "a");
    let mut pv = XlsxPreview::load(&mut ed.model, &book, false).unwrap();
    pv.rev = ed.content_snap();
    assert!(!is_foreign(&pv) && pv.error.is_none());
    let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En);
    assert!(matches!(r, Ok(Applied::Done { .. })), "{r:?}");
    assert!((heights(&ed)[0] - 4.0).abs() < 1e-9);
}

#[test]
fn book_without_project_reads_as_before_with_a_note() {
    let d = dir("no_uid");
    let mut ed = editor();
    let uid = model_uid(&ed.model).unwrap();
    // an older export: the model had no uid yet, so the book names none
    ed.model.sql().execute("DELETE FROM meta WHERE key='project_uid'", []).unwrap();
    let book = edited_book(&ed, &d, "a");
    ed.model.sql().execute("INSERT INTO meta(key, value) VALUES('project_uid', ?1)", [uid]).unwrap();
    assert_eq!(xlsx::book_project(&book).unwrap(), None);
    assert_eq!(origin(&ed.model, &book), Origin::Unknown);
    let mut pv = XlsxPreview::load(&mut ed.model, &book, false).unwrap();
    pv.rev = ed.content_snap();
    assert!(pv.report.no_project, "the preview carries the muted note");
    assert!(!is_foreign(&pv));
    assert_eq!(pv.report.counts().changed, 1);
    // today's path: preview, then Apply
    let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En);
    assert!(matches!(r, Ok(Applied::Done { .. })), "{r:?}");
    assert!((heights(&ed)[0] - 4.0).abs() < 1e-9);
}

#[test]
fn project_swap_forgets_the_link_and_sync_offers_an_export() {
    let d = dir("swap");
    let mut a = app(editor());
    let book = edited_book(&a.ed, &d, "a");
    a.xlsx_open_now(&book, false);
    assert_eq!(last_book().as_deref(), Some(book.as_path()));
    a.xlsx = None;
    assert_eq!(sync_action(false, last_book().map(|p| (p, false))), SyncAction::Open(book.clone(), false));
    // another project is open now; the old one's stored link (meta) went with it
    a.ed = editor();
    crate::project_ui::after_swap(&mut a);
    assert_eq!(last_book(), None);
    assert_eq!(sync_action(a.xlsx.is_some(), last_book().map(|p| (p, false))), SyncAction::OfferExport);
    a.xlsx_sync();
    assert!(a.xlsx.is_none(), "the previous project's book is not opened");
    assert!(a.xlsx_offer);
}

#[test]
fn refusal_texts_exist_in_both_languages() {
    let p = PathBuf::from("c:/books/plan-a.xlsx");
    let (en, ru) = (body(&p, false), body(&p, true));
    assert!(en.contains("plan-a.xlsx") && ru.contains("plan-a.xlsx"));
    assert_ne!(en, ru);
    for k in ["xlsx_project.title", "xlsx_project.export_new", "xlsx_project.close", "xlsx_project.no_project"] {
        let (e, r) = (crate::tr_str(false, k), crate::tr_str(true, k));
        assert!(e != k && r != k, "{k} missing");
        assert_ne!(e, r, "{k} not translated");
    }
    assert_eq!(book_error(FOREIGN, Lang::Ru), crate::tr_str(true, "xlsx_project.title"));
    assert_eq!(book_error(FOREIGN, Lang::En), crate::tr_str(false, "xlsx_project.title"));
}
