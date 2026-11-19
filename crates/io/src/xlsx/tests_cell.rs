// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Cell limits: over-long text and non-finite numbers are written safely on export, and a cut
//! cell read back is never applied over the full model value.

use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::Model;
use rust_xlsxwriter::Workbook;

use super::cellsafe::{MAX_CELL_UNITS, TRUNC_MARK};
use super::*;

type Grid = Vec<Vec<Data>>;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("ddc_xlsx_tests");
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{}_cell_{name}.xlsx", std::process::id()))
}

fn grid(p: &Path, sheet: &str) -> Grid {
    let mut wb: Xlsx<_> = open_workbook(p).unwrap();
    wb.worksheet_range(sheet).unwrap().rows().map(|r| r.to_vec()).collect()
}

/// Copies a book with the grid of every sheet passed through `f` (a stand-in for editing).
fn rewrite(src: &Path, dst: &Path, f: impl Fn(&str, &mut Grid)) {
    let wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = Workbook::new();
    for name in wb.sheet_names() {
        let mut g = grid(src, &name);
        f(&name, &mut g);
        let ws = out.add_worksheet();
        ws.set_name(&name).unwrap();
        for (i, row) in g.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::String(t) => ws.write_string(i, j, t).map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(dst).unwrap();
}

fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

fn long_name() -> String {
    "ab\u{1F600}".repeat(MAX_CELL_UNITS / 2)
}

fn model_with(name: &str) -> Model {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level(name, 0.0).unwrap();
    m
}

fn level_name(m: &Model) -> String {
    m.sql().query_row("SELECT name FROM level", [], |r| r.get(0)).unwrap()
}

fn col_of(g: &Grid, key: &str) -> usize {
    g[0].iter()
        .position(|d| matches!(d, Data::String(s) if s.starts_with(key)))
        .unwrap_or_else(|| panic!("no column {key}"))
}

#[test]
fn long_text_roundtrip_keeps_model_value() {
    let full = long_name();
    assert!(units(&full) > MAX_CELL_UNITS);
    let mut m = model_with(&full);
    let p = tmp("long");
    export_xlsx(&m, &p).unwrap();
    let notes = take_export_notes();
    assert!(
        notes.iter().any(|n| n.kind == ExportNoteKind::Clipped && n.sheet == LEVELS && n.id.is_some()),
        "{notes:?}"
    );
    let g = grid(&p, LEVELS);
    let Data::String(cell) = &g[1][col_of(&g, "name")] else { panic!("name cell") };
    assert!(units(cell) <= MAX_CELL_UNITS && cell.ends_with(TRUNC_MARK));
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.changes.is_empty() && rep.rejected.is_empty(), "{rep:?}");
    assert!(rep.ignored.is_empty(), "{:?}", rep.ignored);
    assert_eq!(level_name(&m), full);
}

#[test]
fn text_at_the_limit_is_written_as_is() {
    let exact = "x".repeat(MAX_CELL_UNITS);
    let mut m = model_with(&exact);
    let p = tmp("limit");
    export_xlsx(&m, &p).unwrap();
    assert!(take_export_notes().is_empty());
    let g = grid(&p, LEVELS);
    assert!(matches!(&g[1][col_of(&g, "name")], Data::String(s) if *s == exact));
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.changes.is_empty() && rep.ignored.is_empty(), "{rep:?}");
    assert_eq!(level_name(&m), exact);
}

#[test]
fn nan_and_inf_exported_as_empty_and_reported() {
    let mut m = model_with("L1");
    m.sql().execute("UPDATE level SET elevation=9e999", []).unwrap();
    let p = tmp("inf");
    export_xlsx(&m, &p).unwrap();
    let notes = take_export_notes();
    assert!(
        notes.iter().any(|n| n.kind == ExportNoteKind::NonFinite && n.sheet == LEVELS),
        "{notes:?}"
    );
    let g = grid(&p, LEVELS);
    assert!(matches!(g[1][col_of(&g, "elevation")], Data::Empty));
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(
        !rep.changes.iter().any(|c| c.sheet == LEVELS && c.column.starts_with("elevation")),
        "{rep:?}"
    );
    let e: f64 = m.sql().query_row("SELECT elevation FROM level", [], |r| r.get(0)).unwrap();
    assert!(e.is_infinite());
}

#[test]
fn truncated_import_refused_with_reason() {
    let full = long_name();
    let mut m = model_with(&full);
    let p = tmp("trunc");
    export_xlsx(&m, &p).unwrap();
    take_export_notes();
    // the cut form with one character changed, and a cell another program cut at the limit
    for (tag, edited) in [
        ("marker", Some(format!("Z{}", &long_name()[1..]))),
        ("bare", None),
    ] {
        let q = tmp(&format!("trunc_{tag}"));
        rewrite(&p, &q, |sheet, g| {
            if sheet != LEVELS {
                return;
            }
            let j = col_of(g, "name");
            let new = match &edited {
                Some(t) => super::cellsafe::clipped(t).unwrap(),
                None => "q".repeat(MAX_CELL_UNITS),
            };
            g[1][j] = Data::String(new);
        });
        let rep = import_xlsx(&mut m, &q).unwrap();
        assert!(
            rep.ignored.iter().any(|i| i.reason == CLIPPED_REASON && i.sheet == LEVELS),
            "{tag}: {rep:?}"
        );
        assert!(!rep.changes.iter().any(|c| c.sheet == LEVELS), "{tag}: {rep:?}");
        assert_eq!(level_name(&m), full, "{tag}");
    }
}

#[test]
fn cut_helper_rules() {
    use super::cellsafe::{clipped, cut_cell, looks_clipped};
    let full = long_name();
    let cut = clipped(&full).unwrap();
    assert!(looks_clipped(&cut) && units(&cut) <= MAX_CELL_UNITS);
    assert_eq!(cut_cell(&cut, Some(&full)), Some(true));
    assert_eq!(cut_cell("other", Some(&full)), Some(false));
    assert_eq!(cut_cell("short", Some("short")), None);
    assert_eq!(cut_cell("new text", None), None);
    assert!(!looks_clipped(&format!("a{TRUNC_MARK}")));
}
