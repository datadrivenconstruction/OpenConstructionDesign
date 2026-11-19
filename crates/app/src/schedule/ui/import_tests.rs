//! Import flow of the schedule view: read off the UI thread, Cancel, a book saved again after
//! the preview, the preview list.

use super::*;
use crate::schedule::xlsx_in::Why;
use ddc_io::xlsx::{SheetCell, TableCell};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

const U: Units = Units::Metric;

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ddc_sch3v_{tag}_{}.xlsx", std::process::id()))
}

/// A room of walls, the view on its walls table, and the keyed export of it at `p`.
fn setup(tag: &str) -> (Editor, View, std::path::PathBuf) {
    let mut ed = Editor::new_in_memory().unwrap();
    crate::schedule::test_room(&mut ed);
    let mut v = View::default();
    let tb = crate::schedule::build(&ed, Cat::Walls);
    let ord = crate::schedule::order(&tb, &[], None, U);
    let cols: Vec<usize> = (0..tb.cols.len()).collect();
    let p = tmp(tag);
    crate::schedule::export_xlsx_keyed(&ed.model, &tb, &ord, &cols, U, &p, &[]).unwrap();
    v.set_table(tb, U);
    (ed, v, p)
}

/// Polls until the read ends, then hands it to `import_read` as the action handler does.
fn finish_read(v: &mut View, ed: &Editor) -> Option<(String, bool)> {
    let t = Instant::now();
    loop {
        let mut acts = vec![];
        v.poll_read(&mut acts);
        if let Some(Action::Import(path)) = acts.pop() {
            return v.import_read(ed, &path, U);
        }
        assert!(v.is_reading(), "the read ended without a result");
        assert!(t.elapsed() < Duration::from_secs(30), "the read never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Rewrite the schedule sheet of `p` with the wall height of its first row set to `mm`, and
/// move the file time on so the save is seen whatever the clock resolution.
fn resave_height(p: &Path, tb: &Table, mm: f64) {
    let sheet: String = tb.cat.title().chars().take(31).collect();
    let (_, mut grid) = ddc_io::xlsx::read_sheet_cells(p, Some(&sheet)).unwrap();
    let k = tb.cols.iter().position(|c| c.key == "h").unwrap();
    let h = super::super::header(&tb.cols[k], U);
    let jh = grid[0].iter().position(|c| matches!(c, SheetCell::Text(t) if *t == h)).unwrap();
    let id = SheetCell::Num(tb.rows[0].id as f64);
    let r = grid.iter().position(|row| row.first() == Some(&id)).unwrap();
    grid[r][jh] = SheetCell::Num(mm);
    let text = |c: &SheetCell| match c {
        SheetCell::Num(x) => TableCell::Num(*x),
        SheetCell::Text(t) => TableCell::Text(t.clone()),
        _ => TableCell::Text(String::new()),
    };
    let headers: Vec<String> = grid[0]
        .iter()
        .map(|c| match c {
            SheetCell::Text(t) => t.clone(),
            _ => String::new(),
        })
        .collect();
    let rows: Vec<Vec<TableCell>> =
        grid[1..].iter().map(|r| r.iter().map(text).collect()).collect();
    ddc_io::xlsx::export_table(p, &sheet, &headers, &[], &rows, None).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(5)).unwrap();
}

fn first_height(ed: &Editor) -> Val {
    let tb = crate::schedule::build(ed, Cat::Walls);
    let k = tb.cols.iter().position(|c| c.key == "h").unwrap();
    tb.rows[0].vals[k].clone()
}

#[test]
fn import_reads_off_the_ui_thread_then_previews() {
    let (ed, mut v, p) = setup("bg");
    assert!(v.import_read(&ed, &p, U).is_none());
    assert!(v.is_reading() && v.import.is_none(), "the pick only starts the read");
    assert!(finish_read(&mut v, &ed).is_none());
    let pend = v.import.as_ref().expect("preview open");
    assert_eq!(pend.plan.changed(), 0);
    assert!(pend.note.is_none() && pend.stamp.is_some());
    let _ = std::fs::remove_file(&p);
}

#[test]
fn cancel_leaves_no_pending() {
    let (ed, mut v, p) = setup("cancel");
    assert!(v.import_read(&ed, &p, U).is_none());
    v.cancel_read();
    assert!(!v.is_reading() && v.import.is_none());
    // whatever the worker still produces is dropped
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(300) {
        let mut acts = vec![];
        v.poll_read(&mut acts);
        assert!(acts.is_empty());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(v.import.is_none(), "no preview after Cancel");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn book_without_the_schedule_sheet_opens_no_preview() {
    let (ed, mut v, p) = setup("nosheet");
    let other = tmp("nosheet_other");
    let headers = vec!["Id".to_string(), "x".to_string()];
    let rows = vec![vec![TableCell::Num(1.0), TableCell::Text("y".into())]];
    ddc_io::xlsx::export_table(&other, "Elsewhere", &headers, &[], &rows, None).unwrap();
    assert!(v.import_read(&ed, &other, U).is_none());
    let (msg, warn) = finish_read(&mut v, &ed).expect("a status");
    assert!(warn && v.import.is_none(), "{msg}");
    let sheet: String = v.table.as_ref().unwrap().cat.title().chars().take(31).collect();
    assert!(msg.contains(&sheet), "{msg}");
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(&other);
}

#[test]
fn schedule_import_apply_after_book_resaved_rereads() {
    let (mut ed, mut v, p) = setup("resave");
    assert!(v.import_read(&ed, &p, U).is_none());
    assert!(finish_read(&mut v, &ed).is_none());
    assert_eq!(v.import.as_ref().unwrap().plan.changed(), 0);
    // saved again in the spreadsheet app while the preview is open
    let tb = v.table.clone().unwrap();
    resave_height(&p, &tb, 3456.0);
    let (rev, h0) = (ed.revision, first_height(&ed));
    let (msg, warn) = v.import_apply(&mut ed, U);
    assert!(warn, "{msg}");
    assert_eq!(msg, crate::tr("sched_import.resaved_reading", &[]));
    assert_eq!((ed.revision, first_height(&ed)), (rev, h0), "nothing stale is applied");
    assert!(v.is_reading() && v.import.is_none());
    assert!(finish_read(&mut v, &ed).is_none());
    let pend = v.import.as_ref().expect("the refreshed preview");
    assert_eq!(pend.note.as_deref(), Some(crate::tr("sched_import.resaved", &[]).as_str()));
    assert_eq!(pend.plan.changed(), 1, "{:?}", pend.plan.refusals);
    let (msg, _) = v.import_apply(&mut ed, U);
    match first_height(&ed) {
        Val::N(x) => assert!((x - 3.456).abs() < 1e-9, "{x} {msg}"),
        other => panic!("{other:?} {msg}"),
    }
    let _ = std::fs::remove_file(&p);
}

#[test]
fn preview_list_rows_for_a_three_cell_plan() {
    use ddc_ui::diff_kind::DiffKind;
    let mut pl = crate::schedule::xlsx_in::Plan::default();
    pl.sheet = "Walls".into();
    for (i, (b, a)) in [("3000", "3500"), ("", "x"), ("1", "2")].into_iter().enumerate() {
        pl.changes.push(crate::schedule::xlsx_in::CellChange {
            row: i as u32 + 2,
            col: 3,
            element: format!("{}", 10 + i),
            column: "Height".into(),
            before: b.into(),
            after: a.into(),
        });
    }
    pl.refusals.push(crate::schedule::xlsx_in::Refusal {
        row: 9,
        col: 2,
        header: "Mark".into(),
        text: "#REF!".into(),
        why: Why::FormulaError,
    });
    let rows = import::list_rows(&pl);
    assert_eq!(rows.len(), 4);
    assert!(rows[..3].iter().all(|(k, _)| *k == Some(DiffKind::Changed)));
    assert_eq!(rows[3].0, Some(DiffKind::Refused));
    assert!(rows[0].1.contains("Walls!C2") && rows[0].1.contains("3500"), "{}", rows[0].1);
    assert!(rows[1].1.contains(&crate::tr("sched_import.blank", &[])), "an empty before is named");
    assert!(rows[3].1.contains("Walls!B9"), "{}", rows[3].1);
}

#[test]
fn preview_list_is_capped_with_the_rest_counted() {
    let mut pl = crate::schedule::xlsx_in::Plan::default();
    let n = import::LIST_CAP + 7;
    pl.changes = (0..n)
        .map(|i| crate::schedule::xlsx_in::CellChange {
            row: i as u32 + 2,
            col: 2,
            element: i.to_string(),
            column: "Height".into(),
            before: "1".into(),
            after: "2".into(),
        })
        .collect();
    let rows = import::list_rows(&pl);
    assert_eq!(rows.len(), import::LIST_CAP + 1);
    assert_eq!(rows.last().unwrap(), &(None, crate::tr("sched_import.more", &[("n", "7")])));
}
