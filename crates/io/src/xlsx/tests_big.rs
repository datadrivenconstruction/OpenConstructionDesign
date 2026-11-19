// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Import progress and cancel hook, and big books.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use calamine::Data;
use ddc_model::{LayerSpec, Model, WallSpec};

use super::tests::{col, edit, set, tmp};
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

fn wall_h(m: &Model, id: i64) -> f64 {
    m.sql()
        .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

fn walls(m: &Model) -> i64 {
    m.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap()
}

fn dry() -> ImportOptions {
    ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    }
}

/// A book of the model with `n` extra id-less Walls rows.
fn book_with(m: &Model, name: &str, n: usize) -> PathBuf {
    let src = tmp(&format!("{name}_src"));
    let dst = tmp(name);
    export_xlsx(m, &src).unwrap();
    edit(&src, &dst, |sheet, g| {
        if sheet != WALLS {
            return;
        }
        let (id, ax, bx) = (col(g, "id"), col(g, "ax [m]"), col(g, "bx [m]"));
        let tpl = g[1].clone();
        for i in 0..n {
            let mut r = tpl.clone();
            r[id] = Data::Empty;
            r[ax] = Data::Float(10.0 + i as f64 * 0.01);
            r[bx] = Data::Float(15.0 + i as f64 * 0.01);
            g.push(r);
        }
    });
    let _ = std::fs::remove_file(&src);
    dst
}

fn counting(calls: &Arc<Mutex<Vec<f32>>>) -> Hook {
    let c = calls.clone();
    Arc::new(move |f| {
        c.lock().unwrap().push(f);
        true
    })
}

#[test]
fn import_cancel_leaves_model_unchanged() {
    let mut m = model();
    let src = tmp("big_cancel_src");
    let p = tmp("big_cancel");
    export_xlsx(&m, &src).unwrap();
    edit(&src, &p, |sheet, g| {
        if sheet == WALLS {
            set(g, 1, "height [m]", Data::Float(9.0));
        }
    });
    let n = AtomicUsize::new(0);
    let n = Arc::new(n);
    let c = n.clone();
    // lets the first sheet start, stops at the second
    let hook: Hook = Arc::new(move |_| c.fetch_add(1, Ordering::Relaxed) < 1);
    let o = ImportOptions { apply: true, ..dry() };
    let r = import_xlsx_hooked(&mut m, &p, o, Some(hook));
    assert_eq!(r.err(), Some(crate::job::CANCELLED.to_string()));
    assert!((wall_h(&m, 1) - 3.0).abs() < 1e-12, "cancelled import changed the model");
    assert_eq!(walls(&m), 2);
    // the hook does not leak into the next import
    let rep = import_xlsx(&mut m, &p).unwrap();
    assert!(rep.applied, "{rep:?}");
    assert!((wall_h(&m, 1) - 9.0).abs() < 1e-12);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn import_progress_per_sheet_and_every_2000_rows() {
    let mut m = model();
    let small = book_with(&m, "big_prog_small", 0);
    let few = Arc::new(Mutex::new(vec![]));
    import_xlsx_hooked(&mut m, &small, dry(), Some(counting(&few))).unwrap();
    let few = few.lock().unwrap().clone();
    assert!(few.len() >= 3, "one call per sheet, got {few:?}");
    assert!(few.windows(2).all(|w| w[0] <= w[1]), "progress goes back: {few:?}");
    assert!(few.iter().all(|f| (0.0..1.0).contains(f)));

    let big = book_with(&m, "big_prog_5k", 5000);
    let many = Arc::new(Mutex::new(vec![]));
    import_xlsx_hooked(&mut m, &big, dry(), Some(counting(&many))).unwrap();
    let many = many.lock().unwrap().clone();
    // 5002 Walls rows: asked at rows 2000 and 4000 besides the sheet starts
    assert_eq!(many.len(), few.len() + 2, "{many:?}");
    assert!(many.windows(2).all(|w| w[0] <= w[1]), "progress goes back: {many:?}");
}

fn read_budget(rows: usize, budget: Duration) {
    let mut m = model();
    let p = book_with(&m, &format!("big_read_{rows}"), rows);
    let t = Instant::now();
    let rep = import_xlsx_with(&mut m, &p, dry()).unwrap();
    let took = t.elapsed();
    assert!(took < budget, "{rows} rows read in {took:?}, budget {budget:?}");
    assert!(!rep.applied);
    assert_eq!(walls(&m), 2, "a dry run changed the model");
    assert!(rep.counts().added > 0 || !rep.edits.is_empty(), "new rows not read");
}

#[test]
fn big_book_50k_read_budget() {
    read_budget(50_000, Duration::from_secs(120));
}

#[test]
#[ignore = "release: cargo test -p ddc-io --release -- --ignored big_book_200k_release"]
fn big_book_200k_release() {
    read_budget(200_000, Duration::from_secs(60));
}
