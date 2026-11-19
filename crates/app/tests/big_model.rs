// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Large synthetic model: a grid of wall-bounded rooms with network segments in each cell.
//! Prints: scene build, operation -> ready scene (partial rebuild), splitting into chunks
//! and how many chunks would be uploaded to the GPU, index build, ray picking, box selection.
//! Full size (~50k elements, ~5M triangles), in release:
//! cargo test -p ddc-app --release --test big_model -- --ignored --nocapture

use ddc_app::editor::{Editor, OpFn, Sel};
use ddc_app::{mep3d, spatial};
use ddc_model::{MepDomain, MepSection, MepShape, Model, RunSpec, SystemKind, WallSpec};
use ddc_render::chunks;
use ddc_render::glam::Vec3;
use ddc_render::{Aabb, Ray};
use std::sync::Arc;
use std::time::Instant;

const CELL: f64 = 6.0;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// k×k rooms, `per` pipe segments in each, `systems` systems.
fn populate(m: &mut Model, level: i64, wt: i64, k: usize, per: usize, systems: usize) -> i64 {
    let mut ws = vec![];
    let w = |a: (f64, f64), b: (f64, f64)| WallSpec {
        a,
        b,
        level,
        base_offset: 0.0,
        height: 3.5,
        wall_type: wt,
        offset0: None,
    };
    for i in 0..=k {
        let c = i as f64 * CELL;
        for j in 0..k {
            let (a, b) = (j as f64 * CELL, (j + 1) as f64 * CELL);
            ws.push(w((a, c), (b, c)));
            ws.push(w((c, a), (c, b)));
        }
    }
    m.add_walls(&ws).unwrap();
    let sys: Vec<i64> = (0..systems)
        .map(|i| {
            let kind = [
                SystemKind::ColdWater,
                SystemKind::HotWater,
                SystemKind::Heating,
            ][i % 3];
            m.add_mep_system(&format!("S{i}"), kind).unwrap()
        })
        .collect();
    let mut specs = vec![];
    for i in 0..k {
        for j in 0..k {
            let (x0, y0) = (i as f64 * CELL, j as f64 * CELL);
            for r in 0..per {
                let y = y0 + 0.6 + r as f64 * (4.8 / per.max(1) as f64);
                let mut s = MepSection::new(MepDomain::Pipe, MepShape::Round { d: 0.08 });
                s.wall = 0.003;
                if r % 2 == 0 {
                    s.insulation = 0.02;
                }
                specs.push(RunSpec::new(
                    sys[(i * k + j) % systems],
                    level,
                    s,
                    vec![(x0 + 0.5, y, 2.8), (x0 + CELL - 0.5, y, 2.8)],
                ));
            }
        }
    }
    m.batch_walls("wall_bench_mep", |db| {
            let mut first = None;
            for s in &specs {
                let id = ddc_model::insert_mep_run(db, s)?;
                first.get_or_insert(id);
            }
            Ok(first.unwrap_or(0))
        })
    .unwrap()
}

/// Rooms and pipes in a new in-memory editor.
fn build(k: usize, per: usize, systems: usize) -> (Editor, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let (level, wt) = (ed.level, ed.types[0].id);
    let first = populate(&mut ed.model, level, wt, k, per, systems);
    ed.reload().unwrap();
    (ed, first)
}

struct Report {
    elements: usize,
    triangles: usize,
    op_to_scene_ms: f64,
    arch_reused: bool,
    changed_chunks: usize,
    wall_add_chunks: usize,
    arch_kept: usize,
    pick_us: f64,
    picks_agree: bool,
}

fn measure(k: usize, per: usize, systems: usize) -> Report {
    let t = Instant::now();
    let (mut ed, run) = build(k, per, systems);
    let t_model = ms(t);

    let t = Instant::now();
    let sc = mep3d::scene(&mut ed);
    let t_scene = ms(t);
    let triangles: usize = sc.meshes.iter().map(|m| m.triangles.len()).sum();
    let elements = ed.walls.len() + per * k * k;
    let t = Instant::now();
    let p0 = chunks::plan(&sc);
    let t_plan = ms(t);
    let t = Instant::now();
    let ix = spatial::Index::build(&sc);
    let t_index = ms(t);

    // ray picking: diagonally from above across the whole model
    let b = sc.bounds.unwrap();
    let n = 2000;
    let rays: Vec<Ray> = (0..n)
        .map(|i| {
            let f = |s: u32| ((i as u32).wrapping_mul(s) % 1000) as f32 / 1000.0;
            let p = b.min + (b.max - b.min) * Vec3::new(f(7919), f(104_729), 1.0);
            Ray {
                origin: p + Vec3::new(-3.0, -4.0, 6.0),
                dir: Vec3::new(0.3, 0.4, -1.0).normalize(),
            }
        })
        .collect();
    let t = Instant::now();
    let hits: Vec<_> = rays.iter().map(|r| ix.pick(&sc, r)).collect();
    let pick_us = t.elapsed().as_secs_f64() * 1e6 / n as f64;
    let check = rays.len().min(if triangles > 1_000_000 { 20 } else { 200 });
    let t = Instant::now();
    let agree = rays[..check]
        .iter()
        .zip(&hits)
        .all(|(r, h)| sc.pick(r).map(|x| x.element) == h.map(|x| x.element));
    let brute_us = t.elapsed().as_secs_f64() * 1e6 / check as f64;
    let t = Instant::now();
    let c = (b.min + b.max) * 0.5;
    let boxed = ix.in_box(&Aabb {
        min: c - Vec3::new(10.0, 10.0, 10.0),
        max: c + Vec3::new(10.0, 10.0, 10.0),
    });
    let box_us = t.elapsed().as_secs_f64() * 1e6;

    // operation: resize one segment's section; then scene, index and chunk split
    let mut s = ed.model.mep_run(run).unwrap();
    s.section.shape = MepShape::Round { d: 0.1 };
    let t0 = Instant::now();
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.update_mep_run(run, &s)
    })))
    .unwrap();
    let t_op = ms(t0);
    let t = Instant::now();
    let sc2 = mep3d::scene(&mut ed);
    let t_scene2 = ms(t);
    let t = Instant::now();
    let ix2 = spatial::Index::update(Some(&ix), &sc2);
    let t_index2 = ms(t);
    let t = Instant::now();
    let p1 = chunks::plan(&sc2);
    let t_plan2 = ms(t);
    let op_to_scene = ms(t0);
    let old: std::collections::HashMap<u64, u64> = p0.iter().map(|p| (p.key, p.stamp)).collect();
    let changed: Vec<_> = p1
        .iter()
        .filter(|p| old.get(&p.key) != Some(&p.stamp))
        .collect();
    let up_tris: usize = changed.iter().map(|p| p.triangles).sum();
    let tm = ed.big.timing.clone();
    let (ra, rb) = ed.element_range(Sel::Mep(run)).unwrap();
    assert!(sc2.meshes.iter().any(|m| (ra..rb).contains(&m.element)));
    assert!(ix2.matches(&sc2));
    let agree2 = rays[..check]
        .iter()
        .all(|r| sc2.pick(r).map(|x| x.element) == ix2.pick(&sc2, r).map(|x| x.element));

    // new wall off to the side: architecture is rebuilt, previous wall numbers are unchanged,
    // so only the new wall's chunk is uploaded to the GPU
    let far = b.max.x as f64 + 40.0;
    let wall = WallSpec {
        a: (far, 0.0),
        b: (far + 3.0, 0.0),
        level: ed.level,
        base_offset: 0.0,
        height: 3.5,
        wall_type: ed.types[0].id,
        offset0: None,
    };
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.add_wall(&wall).map(|_| ())
    })))
    .unwrap();
    let sc3 = mep3d::scene(&mut ed);
    let arch_kept = ed.big.timing.arch_meshes_kept;
    let p2 = chunks::plan(&sc3);
    let old1: std::collections::HashMap<u64, u64> = p1.iter().map(|p| (p.key, p.stamp)).collect();
    let wall_add_chunks = p2
        .iter()
        .filter(|p| old1.get(&p.key) != Some(&p.stamp))
        .count();
    let t = Instant::now();
    let ix3 = spatial::Index::update(Some(&ix2), &sc3);
    let t_index3 = ms(t);
    assert!(ix3.matches(&sc3));

    println!(
        "big model {k}x{k}: walls {}, mep runs {}, elements {elements}, meshes {}, triangles {triangles}",
        ed.walls.len(),
        per * k * k,
        sc.meshes.len()
    );
    println!("  model build {t_model:.0} ms; first scene {t_scene:.0} ms; chunks {} ({t_plan:.0} ms); index {t_index:.1} ms", p0.len());
    println!(
        "  pick {pick_us:.1} us/ray via index ({brute_us:.0} us brute), agree {agree}; box 20 m: {} items in {box_us:.0} us",
        boxed.len()
    );
    println!(
        "  op -> scene {op_to_scene:.1} ms = op {t_op:.1} + scene {t_scene2:.1} (arch reused {}, systems {}) + index {t_index2:.1} + chunk diff {t_plan2:.1}",
        tm.arch_reused, tm.systems_rebuilt
    );
    println!(
        "  chunks to upload after op: {} of {} ({up_tris} triangles)",
        changed.len(),
        p1.len()
    );
    println!(
        "  add wall: {wall_add_chunks} chunks to upload, {arch_kept} arch meshes kept, index update {t_index3:.1} ms"
    );
    Report {
        elements,
        triangles,
        op_to_scene_ms: op_to_scene,
        arch_reused: tm.arch_reused,
        changed_chunks: changed.len(),
        wall_add_chunks,
        arch_kept,
        pick_us,
        picks_agree: agree && agree2,
    }
}

#[test]
fn big_model_small_partial_rebuild() {
    let r = measure(5, 6, 3);
    assert!(r.arch_reused, "MEP op keeps walls cached");
    assert!(r.picks_agree);
    assert!(
        r.changed_chunks >= 1 && r.changed_chunks <= 3,
        "{}",
        r.changed_chunks
    );
    assert!(
        r.wall_add_chunks == 1,
        "only the new wall's chunk: {}",
        r.wall_add_chunks
    );
    assert!(r.arch_kept > 0);
    assert!(r.elements > 100 && r.triangles > 0);
}

#[test]
#[ignore = "full size, run in release with --nocapture"]
fn big_model_50k() {
    let r = measure(70, 10, 60);
    println!(
        "targets: 50k elements ({}), 5M triangles ({}), op->scene < 50 ms ({:.1}), pick < 2 ms ({:.3} ms)",
        r.elements,
        r.triangles,
        r.op_to_scene_ms,
        r.pick_us / 1000.0
    );
}

/// Add of a ~50k-element file: the worker builds the import in a copy of the model file; the
/// UI thread only attaches it, moves the rows in one transaction and reloads.
#[test]
#[ignore = "full size, run in release with --nocapture"]
fn add_ui_stall_under_300ms_at_50k() {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.new_project().unwrap();
    let path = ed.model.sql().path().unwrap_or_default().to_string();
    assert!(!path.is_empty(), "the project must live in a file");
    let wt = ed.types[0].id;
    let t = Instant::now();
    let st = ddc_app::editor::stage::stage(&path, ed.model.change_head(), |m| {
        let l = m.add_level("Imported", 3.0).map_err(|e| e.to_string())?;
        populate(m, l, wt, 70, 10, 60);
        Ok(())
    })
    .unwrap();
    println!("worker (copy + import): {:.0} ms", ms(t));
    let t = Instant::now();
    ed.add_staged(st, "redo".into()).unwrap();
    let stall = ms(t);
    println!("UI thread (attach + insert + reload): {stall:.0} ms");
    assert!(stall < 300.0, "UI stall {stall:.0} ms");
}
