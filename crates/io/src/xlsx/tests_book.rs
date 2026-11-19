// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Professional book: dropdowns, captions with a key row, old books without one.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use calamine::{open_workbook, Data, Reader, Xlsx};
use ddc_model::{LayerSpec, Model, WallSpec};

use super::tests::{edit, set, tmp};
use super::*;

fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let layers = [LayerSpec {
        material: "brick".into(),
        thickness: 0.2,
    }];
    let t = m.add_wall_type("ext", &layers).unwrap();
    for i in 0..2 {
        let w = WallSpec {
            a: (0., i as f64 * 3.),
            b: (5., i as f64 * 3.),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        };
        m.add_wall(&w).unwrap();
    }
    m
}

fn captions() -> HashMap<String, String> {
    [
        ("height", crate::ru_fixtures::ru("height_word")),
        ("level", crate::ru_fixtures::ru("level_word")),
        ("type", crate::ru_fixtures::ru("type_word")),
    ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .into()
}

fn styled(m: &Model, name: &str) -> PathBuf {
    let p = tmp(name);
    export_xlsx_styled(m, &p, &p, Units::Metric, &[], &Style::professional(captions())).unwrap();
    p
}

fn wall_h(m: &Model, id: i64) -> f64 {
    m.sql()
        .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

fn all_sheet_xml(p: &Path) -> String {
    let mut z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
    let mut out = String::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        if f.name().unwrap().starts_with("xl/worksheets/sheet") {
            f.read_to_string(&mut out).unwrap();
        }
    }
    out
}

#[test]
fn book_has_level_dropdown() {
    let m = model();
    let p = styled(&m, "dropdown");
    let x = all_sheet_xml(&p);
    assert!(x.contains("<dataValidation") && x.contains("Lists!$A$2:$A$2"), "{x}");
    assert!(x.contains("Lists!$B$2:$B$2"), "wall type list");
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    let lists = wb.worksheet_range(LISTS).unwrap();
    assert_eq!(lists.get_value((1, 0)).map(|d| d.to_string()), Some("L1".into()));
    let w = zip_workbook(&p);
    assert!(w.contains("state=\"hidden\""), "Lists and Keys are hidden");
    // the plain book stays as it was
    let q = tmp("dropdown_plain");
    export_xlsx(&m, &q).unwrap();
    assert!(!all_sheet_xml(&q).contains("<dataValidation"));
}

fn zip_workbook(p: &Path) -> String {
    let mut z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
    let mut s = String::new();
    z.by_name("xl/workbook.xml").unwrap().read_to_string(&mut s).unwrap();
    s
}

#[test]
fn headers_localized_ru_import_still_maps() {
    let mut m = model();
    let p = styled(&m, "ru_src");
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    let head: Vec<String> = wb
        .worksheet_range(WALLS)
        .unwrap()
        .rows()
        .next()
        .unwrap()
        .iter()
        .map(|d| d.to_string())
        .collect();
    assert!(head.contains(&h().to_string()), "{head:?}");
    assert!(head.contains(&"Base offset [m]".to_string()), "{head:?}");
    assert!(!head.iter().any(|h| h.contains("base_offset")));
    let q = tmp("ru_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, h(),Data::Float(4.0));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert!((wall_h(&m, 1) - 4.0).abs() < 1e-12);
    assert!((wall_h(&m, 2) - 3.0).abs() < 1e-12);
}

#[test]
fn old_book_without_key_row_imports() {
    let mut m = model();
    let p = tmp("old_src");
    export_xlsx(&m, &p).unwrap();
    let mut wb: Xlsx<_> = open_workbook(&p).unwrap();
    assert!(!wb.sheet_names().iter().any(|n| n == KEYS || n == LISTS));
    let q = tmp("old_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 2, "height [m]", Data::Float(3.5));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 1);
    assert!((wall_h(&m, 2) - 3.5).abs() < 1e-12);
}

#[test]
fn baseline_still_matches_after_header_change() {
    let mut m = model();
    let p = styled(&m, "base_src");
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.changes.is_empty() && rep.conflicts.is_empty() && rep.rejected.is_empty(), "{rep:?}");
    let q = tmp("base_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            set(g, 1, h(),Data::Float(4.0));
            set(g, 2, h(),Data::Float(3.2));
        }
    });
    m.sql()
        .execute("UPDATE wall SET height=3.5 WHERE id=1", [])
        .unwrap();
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert_eq!(rep.conflicts.len(), 1, "{:?}", rep.conflicts);
    assert_eq!(rep.conflicts[0].id, Some(1));
    assert!((wall_h(&m, 1) - 3.5).abs() < 1e-12, "conflict not applied");
    assert!((wall_h(&m, 2) - 3.2).abs() < 1e-12);
}

#[test]
fn inserted_column_keeps_keys_on_their_captions() {
    let mut m = model();
    let p = styled(&m, "ins_src");
    let q = tmp("ins_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            for (i, row) in g.iter_mut().enumerate() {
                let v = if i == 0 { Data::String("Note".into()) } else { Data::Empty };
                row.insert(1, v);
            }
            set(g, 1, h(),Data::Float(4.0));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert!((wall_h(&m, 1) - 4.0).abs() < 1e-12);
    assert!((wall_h(&m, 2) - 3.0).abs() < 1e-12);
}

#[test]
fn export_cancel_mid_book_no_partial() {
    let m = model();
    let p = tmp("xl9_cancel");
    let _ = std::fs::remove_file(&p);
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let calls = std::sync::Arc::new(calls);
    let c = calls.clone();
    // lets two sheets through, then stops in the middle of the book
    let hook: Hook = std::sync::Arc::new(move |_| {
        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 2
    });
    let r = export_xlsx_hooked(&m, &p, &p, Units::Metric, &[], &Style::default(), Some(hook));
    assert_eq!(r, Err(CANCELLED.to_string()));
    assert!(calls.load(std::sync::atomic::Ordering::Relaxed) >= 3);
    assert!(!p.exists());
    // the hook state does not leak into the next export
    export_xlsx_styled(&m, &p, &p, Units::Metric, &[], &Style::default()).unwrap();
    assert!(p.exists());
    let _ = std::fs::remove_file(&p);
}

// ---------------------------------------------------------------------------------------------
// columns the user inserted, deleted, moved or renamed in a professional book

type G = Vec<Vec<Data>>;

fn at(g: &G, header: &str) -> usize {
    g[0].iter().position(|d| d.to_string() == header).unwrap()
}

fn insert_col(g: &mut G, at: usize, header: &str, fill: Data) {
    for (r, row) in g.iter_mut().enumerate() {
        while row.len() < at {
            row.push(Data::Empty);
        }
        row.insert(at, if r == 0 { Data::String(header.into()) } else { fill.clone() });
    }
}

fn remove_col(g: &mut G, at: usize) -> Vec<Data> {
    g.iter_mut()
        .map(|r| if at < r.len() { r.remove(at) } else { Data::Empty })
        .collect()
}

fn move_col(g: &mut G, from: usize, to: usize) {
    let c = remove_col(g, from);
    for (r, v) in g.iter_mut().zip(c) {
        let to = to.min(r.len());
        r.insert(to, v);
    }
}

/// The key row as books of older builds wrote it: keys only, no captions.
fn old_keys(sh: &str, g: &mut G) {
    if sh == KEYS {
        g.retain(|r| !r.first().is_some_and(|d| d.to_string().ends_with(":captions")));
    }
}

/// Russian "Height [m]" column header.
fn h() -> &'static str {
    crate::ru_fixtures::ru("height_m")
}

#[test]
fn styled_inserted_column_maps_by_caption() {
    let mut m = model();
    let p = styled(&m, "ins_src");
    let q = tmp("ins_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            // a number column before the height: read by position it would become the height
            let c = at(g, h());
            insert_col(g, c, "Notes", Data::Float(9.0));
            set(g, 1, h(),Data::Float(4.0));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert!((wall_h(&m, 1) - 4.0).abs() < 1e-12);
    assert!((wall_h(&m, 2) - 3.0).abs() < 1e-12, "the inserted column was read as the height");
    assert!(
        rep.proposals.iter().any(|p| p.header == "Notes" && !p.accept),
        "the new column stays the user's: {:?}",
        rep.proposals
    );
}

#[test]
fn styled_deleted_column_reported_never_guessed() {
    let mut m = model();
    let p = styled(&m, "del_src");
    let q = tmp("del_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            let c = at(g, "Base offset [m]");
            remove_col(g, c);
            set(g, 1, h(),Data::Float(4.0));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert!((wall_h(&m, 1) - 4.0).abs() < 1e-12);
    assert!((wall_h(&m, 2) - 3.0).abs() < 1e-12, "a shifted column was read as the height");
    assert!(
        rep.ignored.iter().any(|i| i.sheet == WALLS && i.column == "Base offset [m]"),
        "the deleted column is not reported: {:?}",
        rep.ignored
    );
    // a required column deleted: refused, nothing applied
    let q = tmp("del_req");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            let c = at(g, crate::ru_fixtures::ru("level_word"));
            remove_col(g, c);
            set(g, 2, h(),Data::Float(5.0));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(!rep.applied, "{rep:?}");
    assert!(rep.rejected.iter().any(|r| r.column == crate::ru_fixtures::ru("level_word")), "{:?}", rep.rejected);
    assert!((wall_h(&m, 2) - 3.0).abs() < 1e-12);
}

#[test]
fn styled_moved_column_no_swap() {
    let mut m = model();
    let p = styled(&m, "mov_src");
    let q = tmp("mov_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            let c = at(g, h());
            move_col(g, c, 1);
            let b = at(g, "Base offset [m]");
            let last = g[0].len() - 1;
            move_col(g, b, last);
            set(g, 2, h(),Data::Float(3.3));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert!((wall_h(&m, 1) - 3.0).abs() < 1e-12);
    assert!((wall_h(&m, 2) - 3.3).abs() < 1e-12);
    assert!(rep.proposals.is_empty(), "{:?}", rep.proposals);
}

#[test]
fn styled_renamed_caption_keeps_key() {
    let mut m = model();
    let p = styled(&m, "ren_src");
    let q = tmp("ren_dst");
    edit(&p, &q, |sh, g| {
        if sh == WALLS {
            let c = at(g, h());
            g[0][c] = Data::String("Wall height [m]".into());
            set(g, 1, "Wall height [m]", Data::Float(4.5));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert!((wall_h(&m, 1) - 4.5).abs() < 1e-12);
    assert!(rep.proposals.is_empty(), "a renamed caption became a parameter: {:?}", rep.proposals);
}

#[test]
fn old_key_row_unchanged_imports_by_position() {
    let mut m = model();
    let p = styled(&m, "oldk_src");
    let q = tmp("oldk_dst");
    edit(&p, &q, |sh, g| {
        old_keys(sh, g);
        if sh == WALLS {
            set(g, 1, h(),Data::Float(4.0));
        }
    });
    let keys = {
        let mut wb: Xlsx<_> = open_workbook(&q).unwrap();
        super::book::read_keys(&mut wb)
    };
    assert!(keys[WALLS].captions.is_none(), "the fixture still carries captions");
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(rep.changes.len(), 1, "{:?}", rep.changes);
    assert!((wall_h(&m, 1) - 4.0).abs() < 1e-12);
}

#[test]
fn old_key_row_with_moved_columns_refused_readable() {
    let cases: [(&str, fn(&mut G)); 2] = [
        ("oldk_mov", |g| {
            let c = at(g, h());
            move_col(g, c, 1);
        }),
        ("oldk_ins", |g| {
            let c = at(g, h());
            insert_col(g, c, "Notes", Data::String("n".into()));
        }),
    ];
    for (name, change) in cases {
        let mut m = model();
        let p = styled(&m, name);
        let q = tmp(&format!("{name}_dst"));
        edit(&p, &q, |sh, g| {
            old_keys(sh, g);
            if sh == WALLS {
                change(g);
                set(g, 1, h(),Data::Float(4.0));
            }
        });
        let rep = import_xlsx(&mut m, &q).unwrap();
        assert!(!rep.applied, "{name}: {rep:?}");
        let r = rep
            .rejected
            .iter()
            .find(|r| r.sheet == WALLS && r.reason.contains("older version"))
            .unwrap_or_else(|| panic!("{name}: no readable refusal: {:?}", rep.rejected));
        assert!(r.reason.contains("export the book again"), "{}", r.reason);
        assert!((wall_h(&m, 1) - 3.0).abs() < 1e-12, "{name}: applied");
        assert!((wall_h(&m, 2) - 3.0).abs() < 1e-12, "{name}: applied");
    }
}
