// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Data safety of the schedule import: error cells, formulas without a result and formula
//! results never change the model; rows without Id are named; a full round trip loses nothing.

use super::*;
use crate::schedule::{build, export_xlsx_keyed, order};
use ddc_io::xlsx::{export_table, read_first_of};

fn walls() -> Editor {
    let mut e = Editor::new_in_memory().unwrap();
    crate::schedule::test_room(&mut e);
    e
}

fn tmp(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ddc_sch3s_{tag}_{}.xlsx", std::process::id()))
}

/// Export every row and column keyed, read the schedule sheet back as the import reads it.
fn exported(e: &Editor, tag: &str) -> (Table, std::path::PathBuf, ReadSheet) {
    let tb = build(e, Cat::Walls);
    let ord = order(&tb, &[], None, Units::Metric);
    let cols: Vec<usize> = (0..tb.cols.len()).collect();
    let p = tmp(tag);
    export_xlsx_keyed(&e.model, &tb, &ord, &cols, Units::Metric, &p, &[]).unwrap();
    let s = read_back(&p, &tb);
    (tb, p, s)
}

fn read_back(p: &std::path::Path, tb: &Table) -> ReadSheet {
    let name: String = tb.cat.title().chars().take(31).collect();
    let mut got = read_first_of(p, &[vec![name]], &|| false).unwrap();
    got.remove(0).expect("the schedule sheet")
}

fn col_of(s: &ReadSheet, tb: &Table, key: &str) -> usize {
    let k = tb.cols.iter().position(|c| c.key == key).unwrap();
    let h = header(&tb.cols[k], Units::Metric);
    s.grid[0].iter().position(|c| matches!(c, SheetCell::Text(t) if *t == h)).unwrap()
}

fn text_col(s: &ReadSheet, tb: &Table) -> usize {
    let k = tb.cols.iter().position(|c| c.kind == Kind::Text).unwrap();
    col_of(s, tb, &tb.cols[k].key.clone())
}

fn row_of(s: &ReadSheet, id: i64) -> usize {
    s.grid.iter().position(|r| r.first() == Some(&SheetCell::Num(id as f64))).unwrap()
}

/// Every value of every row, by element and column key (debug text: NaN equals NaN).
fn values(e: &Editor) -> Vec<(i64, u32, String, String)> {
    let tb = build(e, Cat::Walls);
    let mut out = vec![];
    for r in &tb.rows {
        for (k, c) in tb.cols.iter().enumerate() {
            out.push((r.id, r.sub, c.key.to_string(), format!("{:?}", r.vals[k])));
        }
    }
    out
}

fn key_of(p: &std::path::Path) -> Option<Key> {
    Key::read(p)
}

#[test]
fn schedule_xlsx_error_cell_refused() {
    let mut e = walls();
    let (tb, p, mut s) = exported(&e, "err");
    let before = values(&e);
    let (jt, jl) = (text_col(&s, &tb), col_of(&s, &tb, "len"));
    let (ra, rb) = (row_of(&s, tb.rows[0].id), row_of(&s, tb.rows[1].id));
    s.grid[ra][jt] = SheetCell::Error("#REF!".into());
    s.grid[rb][jl] = SheetCell::Error("#DIV/0!".into());
    let pl = plan_sheet(&e, &tb, Units::Metric, &s, key_of(&p).as_ref()).unwrap();
    assert_eq!((pl.changed(), pl.errors), (0, 2), "{:?}", pl.refusals);
    let r = pl.refusals.iter().find(|r| r.row == ra as u32 + 1).unwrap();
    let got = (r.col, r.why.clone(), r.text.as_str());
    assert_eq!(got, (jt as u32 + 1, Why::FormulaError, "#REF!"));
    let line = r.line(&s.name);
    assert!(line.contains(&cell_ref(&s.name, ra as u32 + 1, jt as u32 + 1)), "{line}");
    let o = apply(&mut e, &tb, &pl);
    assert_eq!(o.done, 0);
    assert_eq!(o.bad, 2, "error cells count as skipped");
    assert_eq!(values(&e), before, "an error cell never changes the model");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn schedule_xlsx_uncalculated_formula_not_a_clear() {
    let mut e = walls();
    let (tb, p, mut s) = exported(&e, "nocalc");
    let before = values(&e);
    let (jt, jh) = (text_col(&s, &tb), col_of(&s, &tb, "h"));
    let ra = row_of(&s, tb.rows[0].id);
    s.grid[ra][jt] = SheetCell::Uncalc("=A2&\"x\"".into());
    s.grid[ra][jh] = SheetCell::Uncalc("=2+3".into());
    let pl = plan_sheet(&e, &tb, Units::Metric, &s, key_of(&p).as_ref()).unwrap();
    assert_eq!((pl.changed(), pl.errors), (0, 2), "{:?}", pl.refusals);
    assert!(pl.refusals.iter().all(|r| r.why == Why::Uncalculated), "{:?}", pl.refusals);
    assert!(pl.refusals.iter().any(|r| r.text == "=2+3"), "the formula is shown");
    apply(&mut e, &tb, &pl);
    assert_eq!(values(&e), before, "a formula without a result is never a blank");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn formula_result_is_not_an_edit() {
    let mut e = walls();
    let (tb, p, mut s) = exported(&e, "fres");
    let before = values(&e);
    let (jl, jh) = (col_of(&s, &tb, "len"), col_of(&s, &tb, "h"));
    let ra = row_of(&s, tb.rows[0].id);
    // a result other than the model value, and one equal to it
    s.grid[ra][jl] = SheetCell::Num(7000.0);
    s.formulas.insert((ra, jl));
    s.formulas.insert((ra, jh));
    let pl = plan_sheet(&e, &tb, Units::Metric, &s, key_of(&p).as_ref()).unwrap();
    assert_eq!((pl.changed(), pl.formulas), (0, 1), "{:?}", pl.refusals);
    assert_eq!(pl.refusals.len(), 1, "an equal result is unchanged: {:?}", pl.refusals);
    assert_eq!(pl.refusals[0].why, Why::Formula);
    apply(&mut e, &tb, &pl);
    assert_eq!(values(&e), before);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn error_in_the_id_column_names_no_element() {
    let e = walls();
    let (tb, p, mut s) = exported(&e, "errid");
    let jl = col_of(&s, &tb, "len");
    let ra = row_of(&s, tb.rows[0].id);
    s.grid[ra][0] = SheetCell::Error("#REF!".into());
    s.grid[ra][jl] = SheetCell::Num(9000.0);
    let pl = plan_sheet(&e, &tb, Units::Metric, &s, key_of(&p).as_ref()).unwrap();
    assert_eq!((pl.changed(), pl.errors, pl.unknown_ids), (0, 1, 0), "{:?}", pl.refusals);
    assert_eq!((pl.refusals[0].col, pl.refusals[0].why.clone()), (1, Why::FormulaError));
    let _ = std::fs::remove_file(&p);
}

#[test]
fn error_refusal_text_en_ru() {
    let r = Refusal {
        row: 7,
        col: 4,
        header: "Mark".into(),
        text: "#REF!".into(),
        why: Why::FormulaError,
    };
    let lines: Vec<String> =
        [false, true].iter().map(|&ru| crate::with_test_lang(ru, || r.line("Walls"))).collect();
    for (ru, line) in [false, true].iter().zip(&lines) {
        assert!(line.contains("Walls!D7") && line.contains("#REF!"), "{line}");
        assert!(line.contains(&crate::tr_lang(*ru, "sched_import.why_error", &[])), "{line}");
    }
    assert_ne!(lines[0], lines[1]);
    let u = Refusal { why: Why::Uncalculated, text: "=2+3".into(), ..r };
    for ru in [false, true] {
        let line = crate::with_test_lang(ru, || u.line("Walls"));
        assert!(line.contains(&crate::tr_lang(ru, "sched_import.why_uncalc", &[])), "{line}");
    }
}

#[test]
fn plan_lists_each_cell_before_and_after() {
    let e = walls();
    let (tb, p, mut s) = exported(&e, "list");
    let (jl, jh) = (col_of(&s, &tb, "len"), col_of(&s, &tb, "h"));
    let (ra, rb) = (row_of(&s, tb.rows[0].id), row_of(&s, tb.rows[1].id));
    s.grid[ra][jl] = SheetCell::Num(7000.0);
    s.grid[ra][jh] = SheetCell::Num(3500.0);
    s.grid[rb][jh] = SheetCell::Num(3100.0);
    let pl = plan_sheet(&e, &tb, Units::Metric, &s, key_of(&p).as_ref()).unwrap();
    assert_eq!(pl.changes.len(), 3, "{:?}", pl.refusals);
    assert_eq!(pl.changes.len(), pl.changed());
    let c = &pl.changes[0];
    assert_eq!((c.row, c.col), (ra as u32 + 1, jl as u32 + 1));
    assert_eq!(c.element, tb.rows[0].id.to_string());
    let k = tb.cols.iter().position(|c| c.key == "len").unwrap();
    assert_eq!(c.before, fmt(&tb.rows[0].vals[k], Kind::Len, Units::Metric).trim());
    assert_eq!(c.after, "7000");
    assert_eq!(c.column, header(&tb.cols[k], Units::Metric));
    let line = c.line(&s.name);
    assert!(line.contains(&c.before) && line.contains("7000"), "{line}");
    assert!(line.contains(&c.column), "{line}");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn schedule_import_counts_rows_without_id() {
    let e = walls();
    let (tb, p, mut s) = exported(&e, "noid");
    let jl = col_of(&s, &tb, "len");
    let width = s.grid[0].len();
    let mut typed = vec![SheetCell::Empty; width];
    typed[jl] = SheetCell::Num(4200.0);
    s.grid.push(typed);
    let at = s.grid.len() as u32;
    s.grid.push(vec![SheetCell::Empty; width]);
    let pl = plan_sheet(&e, &tb, Units::Metric, &s, key_of(&p).as_ref()).unwrap();
    assert_eq!(pl.no_id_rows, vec![at], "only the typed row, not the totals or a blank row");
    let line = pl.no_id_line().unwrap();
    assert!(line.contains(&at.to_string()), "{line}");
    assert_eq!(pl.changed(), 0);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn totals_row_is_not_a_row_without_id() {
    let e = walls();
    let (tb, p, s) = exported(&e, "tot");
    // the export writes a totals line with a blank Id
    let blank_id = s.grid.iter().skip(1).filter(|r| r.first().map_or(true, is_blank)).count();
    assert!(blank_id >= 1, "{:?}", s.grid);
    let pl = plan_sheet(&e, &tb, Units::Metric, &s, key_of(&p).as_ref()).unwrap();
    assert!(pl.no_id_rows.is_empty(), "{:?}", pl.no_id_rows);
    assert!(pl.no_id_line().is_none());
    // also with the formulas gone (a book saved as values): the total label marks the line
    let pl = plan_keyed(&e, &tb, Units::Metric, &s.name, &s.grid, key_of(&p).as_ref()).unwrap();
    assert!(pl.no_id_rows.is_empty(), "{:?}", pl.no_id_rows);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn many_rows_without_id_are_capped_in_the_line() {
    let pl = Plan { no_id_rows: (10..30).collect(), ..Plan::default() };
    let line = pl.no_id_line().unwrap();
    assert!(line.contains("10") && line.contains("17") && !line.contains("18"), "{line}");
    assert!(line.contains(&crate::tr("sched_import.more", &[("n", "12")])), "{line}");
    assert!(line.contains("20"), "the count names all: {line}");
}

/// Save `grid` as the user's book (header row + rows), as values.
fn resave(p: &std::path::Path, sheet: &str, grid: &[Vec<SheetCell>]) {
    let to = |c: &SheetCell| match c {
        SheetCell::Num(x) => TableCell::Num(*x),
        other => TableCell::Text(shown(other)),
    };
    let headers: Vec<String> = grid[0].iter().map(shown).collect();
    let rows: Vec<Vec<TableCell>> = grid[1..].iter().map(|r| r.iter().map(to).collect()).collect();
    export_table(p, sheet, &headers, &[], &rows, None).unwrap();
}

#[test]
fn full_round_trip_loses_nothing() {
    let mut e = walls();
    let (tb, p, s) = exported(&e, "full");
    let before = values(&e);
    // unchanged book: nothing to write, nothing refused, nothing unnamed
    let pl = plan_sheet(&e, &tb, Units::Metric, &s, key_of(&p).as_ref()).unwrap();
    assert_eq!((pl.changed(), pl.errors, pl.formulas), (0, 0, 0), "{:?}", pl.refusals);
    assert!(pl.refusals.is_empty() && pl.no_id_rows.is_empty(), "{:?}", pl.refusals);
    assert_eq!(pl.rows, tb.rows.len());
    let rev = e.revision;
    apply(&mut e, &tb, &pl);
    assert_eq!((e.revision, values(&e)), (rev, before.clone()));
    // three edits through a saved book; every other value stays
    let mut g = s.grid.clone();
    let (jl, jh) = (col_of(&s, &tb, "len"), col_of(&s, &tb, "h"));
    let (a, b) = (tb.rows[0].id, tb.rows[1].id);
    let (ra, rb) = (row_of(&s, a), row_of(&s, b));
    g[ra][jl] = SheetCell::Num(7000.0);
    g[ra][jh] = SheetCell::Num(3500.0);
    g[rb][jh] = SheetCell::Num(3100.0);
    resave(&p, &s.name, &g);
    let s2 = read_back(&p, &tb);
    let pl = plan_sheet(&e, &tb, Units::Metric, &s2, None).unwrap();
    assert_eq!(pl.changed(), 3, "{:?}", pl.refusals);
    let o = apply(&mut e, &tb, &pl);
    assert_eq!(o.done, 3, "{:?}", e.status);
    let now = values(&e);
    // length and height also move area and volume of their own wall; other walls stay whole
    assert_eq!(before.len(), now.len());
    for (old, new) in before.iter().zip(&now) {
        assert_eq!((old.0, old.1, &old.2), (new.0, new.1, &new.2));
        if old.0 != a && old.0 != b {
            assert_eq!(old.3, new.3, "{} {} kept", old.0, old.2);
        }
    }
    let h_b = &now.iter().find(|v| v.0 == b && v.2 == "h").unwrap().3;
    assert_eq!(*h_b, format!("{:?}", Val::N(3.1)));
    // one undo step returns every value
    e.undo();
    assert_eq!(values(&e), before);
    let _ = std::fs::remove_file(&p);
}
