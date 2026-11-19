//! Read-back check: what a dry-run report of a fresh book may and may not hold.

use calamine::Data;
use ddc_model::Model;

use super::baseline::{BVal, KeptCell};
use super::tests::{edit, set, tmp};
use super::*;

fn change(sheet: &str, row: u32, col: u32, column: &str, id: i64, old: &str, new: &str) -> Change {
    Change { sheet: sheet.into(), row, col, column: column.into(), id: Some(id), old: old.into(), new: new.into() }
}

fn rejection(sheet: &str, row: u32, col: u32, reason: &str) -> Rejection {
    Rejection { sheet: sheet.into(), row, col, column: "height [m]".into(), reason: reason.into() }
}

fn ignored(sheet: &str, row: u32, id: Option<i64>, reason: &str) -> Ignored {
    Ignored {
        sheet: sheet.into(),
        row,
        col: 1,
        column: "id".into(),
        id,
        old: String::new(),
        new: String::new(),
        reason: reason.into(),
    }
}

fn none() -> Tolerated {
    Tolerated::default()
}

#[test]
fn empty_report_has_no_mismatch() {
    let rep = Report::default();
    assert!(mismatches(&rep, &none()).is_empty());
    assert!(newer(&rep).is_none());
}

#[test]
fn read_only_and_user_columns_are_tolerated() {
    let mut rep = Report::default();
    rep.ignored.push(ignored("Walls", 2, Some(1), "read-only column, computed value is 3; edit not accepted"));
    rep.ignored.push(ignored("Walls", 2, Some(1), CLIPPED_REASON));
    rep.ignored.push(ignored("Doors", 2, Some(5), "column 'my note' is not a field of Doors; not applied"));
    rep.rejected.push(rejection("Walls", 3, 4, "read-only column, computed value is 2; edit not accepted"));
    rep.warnings.push(rejection("Walls", 3, 5, "unusual value"));
    assert!(mismatches(&rep, &none()).is_empty(), "{:?}", mismatches(&rep, &none()));
}

#[test]
fn value_mismatch_names_cell_expected_and_read() {
    let mut rep = Report::default();
    rep.changes.push(change("Walls", 7, 4, "height [m]", 3, "3", "3.5"));
    let m = mismatches(&rep, &none());
    assert_eq!(m.len(), 1);
    let first = &m[0];
    assert_eq!(first.kind, MismatchKind::Value);
    assert_eq!((first.sheet.as_str(), first.row, first.col), ("Walls", 7, 4));
    assert_eq!((first.expected.as_str(), first.read.as_str()), ("3", "3.5"));
    let t = first.message();
    assert!(t.contains("Walls!D7") && t.contains("height [m]") && t.contains("'3'") && t.contains("'3.5'"), "{t}");
    let (_, tpl) = VERIFY_TEMPLATES[0];
    assert!(match_template(tpl, &t).is_some(), "{t}");
}

#[test]
fn row_kinds_are_told_apart() {
    let mut rep = Report::default();
    rep.changes.push(change("Walls", 9, 0, "id", 0, "", "created"));
    rep.changes.push(change("Walls", 4, 0, "id", 2, "", "deleted (row removed)"));
    rep.removed.push(Removed { sheet: "Rooms".into(), id: 11 });
    rep.ignored.push(ignored("Walls", 10, Some(99), "wall 99 no longer exists; row ignored"));
    let m = mismatches(&rep, &none());
    let kinds: Vec<_> = m.iter().map(|x| (x.kind, x.row)).collect();
    assert_eq!(
        kinds,
        vec![
            (MismatchKind::MissingRow, 4),
            (MismatchKind::ExtraRow, 9),
            (MismatchKind::ExtraRow, 10),
            (MismatchKind::MissingRow, 0),
        ]
    );
    assert!(m[1].message().contains("Walls!9:9"), "{}", m[1].message());
    assert!(m[3].message().contains("Rooms") && m[3].message().contains("11"), "{}", m[3].message());
}

#[test]
fn sheet_issue_and_refusal_reported() {
    let mut rep = Report::default();
    rep.rejected.push(rejection("Walls", 5, 4, "number expected, got 'abc'"));
    rep.sheet_issues.push(SheetIssue::missing("Doors"));
    let m = mismatches(&rep, &none());
    assert_eq!(m[0].kind, MismatchKind::Sheet);
    assert_eq!(m[1].kind, MismatchKind::Refused);
    let t = m[1].message();
    assert!(t.contains("Walls!D5") && t.contains("number expected"), "{t}");
}

#[test]
fn first_cell_follows_sheet_then_row_then_column() {
    let mut rep = Report::default();
    rep.changes.push(change("Walls", 8, 2, "name", 1, "a", "b"));
    rep.changes.push(change("Walls", 3, 5, "name", 2, "a", "b"));
    rep.changes.push(change("Levels", 2, 2, "name", 3, "a", "b"));
    rep.changes.push(change("Walls", 3, 2, "name", 2, "a", "b"));
    let m = mismatches(&rep, &none());
    let at: Vec<_> = m.iter().map(|x| x.address()).collect();
    assert_eq!(at, vec!["Walls!B3", "Walls!E3", "Walls!B8", "Levels!B2"]);
}

#[test]
fn kept_rows_and_noted_cells_are_tolerated() {
    let mut rep = Report::default();
    rep.changes.push(change("Walls", 2, 4, "height [m]", 1, "3", "4"));
    rep.conflicts.push(Conflict {
        sheet: "Walls".into(),
        row: 2,
        col: 4,
        column: "height [m]".into(),
        id: Some(1),
        base: "3".into(),
        book: "4".into(),
        model: "3.2".into(),
        take_book: false,
    });
    rep.changes.push(change("Levels", 3, 2, "name", 7, "long", "lo [...]"));
    let kept = vec![KeptCell::new("Walls", Some(1), "height", BVal::N(4.0), BVal::N(3.0)).unwrap()];
    let notes = vec![ExportNote { sheet: "Levels".into(), row: 3, col: 2, id: Some(7), kind: ExportNoteKind::Clipped }];
    let tol = Tolerated::new(&kept, &notes);
    assert!(tol.has_cells());
    assert!(mismatches(&rep, &tol).is_empty(), "{:?}", mismatches(&rep, &tol));
    // another row of the same sheet is still compared
    rep.changes.push(change("Walls", 3, 4, "height [m]", 2, "3", "4"));
    assert_eq!(mismatches(&rep, &tol).len(), 1);
}

#[test]
fn newer_cells_counted() {
    let rep = Report { model_newer: 3, ..Default::default() };
    let n = newer(&rep).unwrap();
    assert_eq!((n.kind, n.count), (MismatchKind::Newer, 3));
    assert!(n.message().contains('3'));
}

#[test]
fn long_text_shown_around_the_first_difference() {
    let a: String = "abc".repeat(4000) + "X" + &"z".repeat(3000);
    let b: String = "abc".repeat(4000) + "Y" + &"z".repeat(3000);
    let (x, y) = shown_pair(&a, &b);
    assert!(x.contains('X') && y.contains('Y'), "{x} / {y}");
    assert!(x.starts_with("...") && x.ends_with("...") && x.chars().count() <= 66, "{x}");
    // multi-byte text is cut on characters, never inside one
    let u: String = "\u{4e2d}\u{00e9}\u{1f600}".repeat(50);
    let v: String = u.clone() + "\u{00df}";
    let (p, q) = shown_pair(&u, &v);
    assert!(p.chars().count() <= 66 && q.contains('\u{00df}'), "{p} / {q}");
    let (s, t) = shown_pair("short", "shorter");
    assert_eq!((s.as_str(), t.as_str()), ("short", "shorter"));
}

#[test]
fn without_baseline_is_scoped() {
    assert!(!verify::plain_read());
    let inner = without_baseline(|| {
        let nested = without_baseline(verify::plain_read);
        (verify::plain_read(), nested)
    });
    assert_eq!(inner, (true, true));
    assert!(!verify::plain_read());
    let r = std::panic::catch_unwind(|| without_baseline(|| panic!("boom")));
    assert!(r.is_err());
    assert!(!verify::plain_read(), "reset after a panic");
}

fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("L1", 0.0).unwrap();
    m.add_level("L2 \u{00fc}\u{4e2d}\u{1f600}", 3.0).unwrap();
    m
}

fn dry(m: &mut Model, p: &std::path::Path) -> Report {
    let o = ImportOptions { apply: false, delete_missing: false, new_params: false };
    import_xlsx_with(m, p, o).unwrap()
}

#[test]
fn fresh_book_reads_back_clean() {
    let mut m = model();
    let p = tmp("xs6_clean");
    export_xlsx(&m, &p).unwrap();
    let rep = dry(&mut m, &p);
    assert!(mismatches(&rep, &none()).is_empty(), "{:?}", mismatches(&rep, &none()));
    assert!(newer(&rep).is_none());
}

#[test]
fn edited_cell_is_named_by_the_dry_run() {
    let mut m = model();
    let p = tmp("xs6_edit_src");
    export_xlsx(&m, &p).unwrap();
    let q = tmp("xs6_edit_dst");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            set(g, 2, "name", Data::String("L2 \u{00fc}\u{4e2d}\u{1f601}".into()));
        }
    });
    let rep = dry(&mut m, &q);
    let first = mismatches(&rep, &none()).into_iter().next().expect("a mismatch");
    assert_eq!((first.kind, first.sheet.as_str(), first.row), (MismatchKind::Value, LEVELS, 3));
    assert_eq!(first.expected, "L2 \u{00fc}\u{4e2d}\u{1f600}");
    assert_eq!(first.read, "L2 \u{00fc}\u{4e2d}\u{1f601}");
}

#[test]
fn wrong_cell_and_baseline_named_without_baseline() {
    let mut m = model();
    let p = tmp("xs6_newer");
    export_xlsx(&m, &p).unwrap();
    // the model moves on: sheet and baseline both hold the old value, as a faulty export would
    m.sql().execute("UPDATE level SET name='L1 moved' WHERE name='L1'", []).unwrap();
    let rep = dry(&mut m, &p);
    assert!(mismatches(&rep, &none()).is_empty());
    assert!(newer(&rep).is_some(), "{rep:?}");
    let plain = without_baseline(|| dry(&mut m, &p));
    let first = mismatches(&plain, &none()).into_iter().next().expect("named without baseline");
    assert_eq!((first.kind, first.expected.as_str(), first.read.as_str()), (MismatchKind::Value, "L1 moved", "L1"));
}
