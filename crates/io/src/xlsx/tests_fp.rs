// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Numeric level names and other numeric identifying cells: one text conversion on both sides.

use calamine::Data;
use ddc_model::Model;

use super::tests::{edit, set, tmp};
use super::*;

type Grid = Vec<Vec<Data>>;

const NAMES: [&str; 4] = ["1", "1.0", "01", "2.50"];

fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    for (i, n) in NAMES.iter().enumerate() {
        m.add_level(n, i as f64 * 3.0).unwrap();
    }
    m
}

fn level_count(m: &Model) -> i64 {
    m.sql().query_row("SELECT COUNT(*) FROM level", [], |r| r.get(0)).unwrap()
}

fn name_col(g: &Grid) -> usize {
    g[0].iter().position(|d| d.to_string() == "name").unwrap()
}

fn export(m: &Model, name: &str) -> std::path::PathBuf {
    let p = tmp(name);
    export_xlsx(m, &p).unwrap();
    p
}

#[test]
fn numeric_level_names_roundtrip_with_zero_changes() {
    let mut m = model();
    let p = export(&m, "fp_names");
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.applied && rep.changes.is_empty() && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(level_count(&m), 4);
}

#[test]
fn numeric_level_name_pairs_record() {
    // a book that stores the names "1" and "2.5" as numbers still pairs with the model
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("1", 0.0).unwrap();
    m.add_level("2.5", 3.0).unwrap();
    let p = export(&m, "fp_num_src");
    let q = tmp("fp_num_dst");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            let c = name_col(g);
            g[1][c] = Data::Float(1.0);
            g[2][c] = Data::Float(2.5);
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.changes.is_empty() && rep.rejected.is_empty(), "{rep:?}");
    assert_eq!(level_count(&m), 2);
}

#[test]
fn numeric_level_second_apply_creates_nothing() {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("L1", 0.0).unwrap();
    let p = export(&m, "fp_second_src");
    let q = tmp("fp_second_dst");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            g.push(vec![Data::Empty; g[0].len()]);
            set(g, 2, "name", Data::Float(3.0));
            set(g, 2, "elevation [m]", Data::Float(6.0));
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.changes.len() == 1, "{rep:?}");
    assert_eq!(level_count(&m), 2);
    let id: i64 = m.sql().query_row("SELECT id FROM level WHERE name='3'", [], |r| r.get(0)).unwrap();
    let mut take = Take::default();
    for ((sheet, row), fp) in &rep.fingerprints {
        take.made.entry((sheet.clone(), fp.clone())).or_default().push((*row, id));
    }
    assert!(!take.made.is_empty());
    let rep = baseline::with_take(&take, || import_xlsx(&mut m, &q)).unwrap();
    assert_eq!(level_count(&m), 2, "{rep:?}");
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
}

#[test]
fn numeric_name_fingerprint_matches_text_name() {
    assert_eq!(baseline::cell_text(&Data::Float(3.0)), baseline::cell_text(&Data::String(" 3 ".into())));
    assert_eq!(baseline::cell_text(&Data::Int(3)), "3");
    assert_eq!(baseline::cell_text(&Data::Float(2.5)), "2.5");
    assert_eq!(level_fp("3"), fingerprint(&[("name", baseline::cell_text(&Data::Float(3.0)))]));
}

#[test]
fn real_rename_of_numeric_level_still_applies() {
    let mut m = model();
    let p = export(&m, "fp_rename_src");
    let q = tmp("fp_rename_dst");
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            let c = name_col(g);
            g[1][c] = Data::String("Ground".into());
        }
    });
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.changes.len() == 1, "{rep:?}");
    let n: String = m.sql().query_row("SELECT name FROM level ORDER BY id LIMIT 1", [], |r| r.get(0)).unwrap();
    assert_eq!(n, "Ground");
}

#[test]
fn claimed_cleared_per_pass() {
    let mut take = Take::default();
    take.claimed.insert((LEVELS.to_string(), "x".into(), 1));
    take.made.insert((LEVELS.to_string(), "x".into()), vec![(1, 7)]);
    let a = baseline::with_take(&take, || baseline::made(LEVELS, 1, "x", "x"));
    let b = baseline::with_take(&take, || baseline::made(LEVELS, 1, "x", "x"));
    assert_eq!((a, b), (Some(7), Some(7)));
}

/// A book with one id-less level row named 3 (stored as a number) on row 2.
fn numeric_new_level_book(m: &mut Model, name: &str) -> std::path::PathBuf {
    m.add_level("L1", 0.0).unwrap();
    let p = export(m, &format!("{name}_src"));
    let q = tmp(&format!("{name}_dst"));
    edit(&p, &q, |sh, g| {
        if sh == LEVELS {
            g.push(vec![Data::Empty; g[0].len()]);
            set(g, 2, "name", Data::Float(3.0));
            set(g, 2, "elevation [m]", Data::Float(6.0));
        }
    });
    q
}

fn level_id(m: &Model, name: &str) -> i64 {
    m.sql().query_row("SELECT id FROM level WHERE name=?1", [name], |r| r.get(0)).unwrap()
}

#[test]
fn legacy_fp_differs_only_for_numeric_text_keys() {
    use super::import::fp_text;
    for k in ["name", "number", "type", "level", "kind"] {
        assert_eq!(fp_text(k, &Data::Float(3.0), false), "3");
        assert_eq!(fp_text(k, &Data::Float(3.0), true), "3.0000");
        assert_eq!(fp_text(k, &Data::Int(3), true), "3.0000");
        let s = Data::String(" Wall A ".into());
        assert_eq!(fp_text(k, &s, false), fp_text(k, &s, true));
    }
    for k in ["ax", "py", "along"] {
        assert_eq!(fp_text(k, &Data::Float(3.0), false), fp_text(k, &Data::Float(3.0), true));
    }
    let legacy = fingerprint(&[("name", fp_text("name", &Data::Float(3.0), true))]);
    assert_ne!(legacy, level_fp("3"));
    assert_eq!(fingerprint(&[("name", fp_text("name", &Data::String("L1".into()), true))]), level_fp("L1"));
}

#[test]
fn reapply_old_record_numeric_name_no_duplicate() {
    let mut m = Model::open(":memory:").unwrap();
    let q = numeric_new_level_book(&mut m, "fp_legacy");
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.changes.len() == 1, "{rep:?}");
    let id = level_id(&m, "3");
    // the record as an older Apply stored it: the name hashed as "3.0000"
    let legacy = fingerprint(&[("name", "3.0000".to_string())]);
    let mut take = Take::default();
    take.made.insert((LEVELS.to_string(), legacy.clone()), vec![(2, id)]);
    let rep = baseline::with_take(&take, || import_xlsx(&mut m, &q)).unwrap();
    assert_eq!(level_count(&m), 2, "{rep:?}");
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
    // claimed once, under the key it was found by
    let fp = level_fp("3");
    let once = || baseline::made(LEVELS, 2, &fp, &legacy);
    let ab = baseline::with_take(&take, || (once(), once()));
    assert_eq!(ab, (Some(id), None));
}

#[test]
fn new_record_matches_current_fp() {
    let mut m = Model::open(":memory:").unwrap();
    let q = numeric_new_level_book(&mut m, "fp_current");
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.changes.len() == 1, "{rep:?}");
    let id = level_id(&m, "3");
    let fps: Vec<&String> = rep.fingerprints.values().collect();
    assert_eq!(fps, vec![&level_fp("3")]);
    let mut take = Take::default();
    take.made.insert((LEVELS.to_string(), level_fp("3")), vec![(2, id)]);
    // a legacy record of another row stays unclaimed: the current key wins first
    let legacy = fingerprint(&[("name", "3.0000".to_string())]);
    take.made.insert((LEVELS.to_string(), legacy.clone()), vec![(2, 99)]);
    let got = baseline::with_take(&take, || baseline::made(LEVELS, 2, &level_fp("3"), &legacy));
    assert_eq!(got, Some(id));
    let rep = baseline::with_take(&take, || import_xlsx(&mut m, &q)).unwrap();
    assert_eq!(level_count(&m), 2, "{rep:?}");
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
}

#[test]
fn record_with_old_numeric_fingerprint_still_matches() {
    // an Apply before numeric names read as text stored "3.0000" in the record
    let mut m = Model::open(":memory:").unwrap();
    let q = numeric_new_level_book(&mut m, "fp_v1");
    let rep = import_xlsx(&mut m, &q).unwrap();
    assert!(rep.applied && rep.changes.len() == 1, "{rep:?}");
    let id = level_id(&m, "3");
    let old = fingerprint(&[("name", "3.0000".to_string())]);
    let mut take = Take::default();
    for (sheet, row) in rep.fingerprints.keys() {
        // another row number too: the record must pair by its fingerprint, not by the row
        take.made.entry((sheet.clone(), old.clone())).or_default().push((*row + 5, id));
    }
    assert!(!take.made.is_empty());
    let fp = level_fp("3");
    let got = baseline::with_take(&take, || baseline::made(LEVELS, 2, &fp, &old));
    assert_eq!(got, Some(id));
    let rep = baseline::with_take(&take, || import_xlsx(&mut m, &q)).unwrap();
    assert_eq!(level_count(&m), 2, "{rep:?}");
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
}
