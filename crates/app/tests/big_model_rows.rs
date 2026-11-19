// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 50k-element timing rows, all ignored: run by tools/bench.ps1 in release.
//! Budgets: open 50k file 3000 ms; XLSX export 15000 ms; XLSX import preview 15000 ms;
//! IFC save 20000 ms; one edit of 1000 elements 1500 ms. DDC_BUDGET_SCALE multiplies every
//! budget (slow machines); the numbers print either way.

use ddc_app::editor::{Editor, OpFn};
use ddc_model::{Model, WallSpec};
use std::sync::Arc;
use std::time::Instant;

const SIDE: usize = 224;
const PITCH: f64 = 6.0;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// About 50k separate walls in one batch.
fn model_50k() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let (level, wt) = (ed.level, ed.types[0].id);
    let ws: Vec<WallSpec> = (0..SIDE * SIDE)
        .map(|i| {
            let (x, y) = ((i % SIDE) as f64 * PITCH, (i / SIDE) as f64 * PITCH);
            WallSpec {
                a: (x, y),
                b: (x + 3.0, y),
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            }
        })
        .collect();
    ed.model.add_walls(&ws).unwrap();
    ed.reload().unwrap();
    assert!(ed.walls.len() >= 50_000, "{}", ed.walls.len());
    ed
}

fn budget_row(name: &str, budget_ms: f64, took_ms: f64) {
    let scale: f64 = std::env::var("DDC_BUDGET_SCALE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0);
    let b = budget_ms * scale;
    println!(
        "{name}: {took_ms:.1} ms, budget {b:.0} ms bench_secs={:.6}",
        took_ms / 1e3
    );
    assert!(took_ms <= b, "{name}: {took_ms:.1} ms over the {b:.0} ms budget");
}

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ddc_big_model_{}_{name}", std::process::id()))
}

#[test]
#[ignore = "50k timing row, run by tools/bench.ps1 in release"]
fn big_open_50k() {
    let ed = model_50k();
    let p = tmp("open.ddc");
    ed.model.snapshot_to(&p).unwrap();
    let t = Instant::now();
    let m = Model::open(p.to_str().unwrap()).unwrap();
    let took = ms(t);
    let n: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
        .unwrap();
    let _ = std::fs::remove_file(&p);
    assert!(n >= 50_000, "{n}");
    budget_row("big_open_50k", 3000.0, took);
}

#[test]
#[ignore = "50k timing row, run by tools/bench.ps1 in release"]
fn big_xlsx_50k() {
    use ddc_io::xlsx::{self, ImportOptions};
    let mut ed = model_50k();
    let p = tmp("book.xlsx");
    let t = Instant::now();
    xlsx::export_xlsx(&ed.model, &p).unwrap();
    let t_export = ms(t);
    let o = ImportOptions {
        apply: false,
        delete_missing: false,
        new_params: false,
    };
    let t = Instant::now();
    let rep = xlsx::import_xlsx_with(&mut ed.model, &p, o).unwrap();
    let t_preview = ms(t);
    let _ = std::fs::remove_file(&p);
    assert!(rep.rejected.is_empty(), "{:?}", rep.rejected.len());
    assert!(rep.changes.is_empty(), "{}", rep.changes.len());
    println!("xlsx export {t_export:.0} ms, import preview {t_preview:.0} ms");
    budget_row("big_xlsx_export_50k", 15_000.0, t_export);
    budget_row("big_xlsx_preview_50k", 15_000.0, t_preview);
}

#[test]
#[ignore = "50k timing row, run by tools/bench.ps1 in release"]
fn big_ifc_save_50k() {
    let ed = model_50k();
    let p = tmp("out.ifc");
    let t = Instant::now();
    let text = ddc_io::ifc::export_ifc(&ed.model).unwrap();
    std::fs::write(&p, &text).unwrap();
    let took = ms(t);
    let _ = std::fs::remove_file(&p);
    assert!(text.len() > 1_000_000, "{}", text.len());
    budget_row("big_ifc_save_50k", 20_000.0, took);
}

#[test]
#[ignore = "50k timing row, run by tools/bench.ps1 in release"]
fn big_multi_edit_50k() {
    let mut ed = model_50k();
    let ids: Vec<i64> = {
        let mut st = ed
            .model
            .sql()
            .prepare("SELECT id FROM wall ORDER BY id LIMIT 1000")
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .map(|x| x.unwrap())
            .collect()
    };
    assert_eq!(ids.len(), 1000);
    let row = ddc_app::geom_props::rows()
        .into_iter()
        .find(|r| r.kind == "wall" && r.key == "geom.height")
        .expect("wall height row");
    let set = row.set;
    let t = Instant::now();
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        ids.iter().try_for_each(|id| set(m, *id, 3.5))
    })))
    .unwrap();
    let took = ms(t);
    let t = Instant::now();
    ed.undo();
    println!("undo of the edit: {:.1} ms", ms(t));
    budget_row("big_multi_edit_50k", 1500.0, took);
}
