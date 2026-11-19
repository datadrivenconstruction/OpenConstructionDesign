// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `--links-bench N [--links-elems E]`: N linked projects of E walls each. Cold: parallel import
//! into the link cache. Warm: parallel cache reads, host scene plus link groups, renderer setup
//! and first frames on this GPU. Then host edits with the links loaded: time and link rebuilds.
//! Prints one JSON line in the bench ledger format.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use web_time::Instant;

use ddc_model::links::Transform;
use ddc_model::WallSpec;

use super::{cache, load, source, Loaded, State};
use crate::editor::Editor;

fn add_walls(ed: &mut Editor, n: usize, y0: f64) -> Result<(), String> {
    let ty = ed.current_type().ok_or("no wall type")?.id;
    let row = 50usize;
    for i in 0..n {
        let (x, y) = ((i % row) as f64 * 2.0, y0 + (i / row) as f64 * 2.0);
        ed.model
            .add_wall(&WallSpec {
                a: (x, y),
                b: (x + 1.5, y),
                level: ed.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: ty,
                offset0: None,
            })
            .map_err(|e| e.to_string())?;
    }
    ed.reload().map_err(|e| e.to_string())
}

/// IFC sources of the corpus, generated once per (n, e) and reused.
fn corpus(dir: &Path, n: usize, e: usize) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut out = vec![];
    for k in 0..n {
        let p = dir.join(format!("link{k:02}.ifc"));
        if !p.is_file() {
            let mut ed = Editor::new_in_memory().map_err(|e| e.to_string())?;
            add_walls(&mut ed, e, k as f64 * 0.5)?;
            let text = ddc_io::ifc::export_ifc(&ed.model)?;
            std::fs::write(&p, text).map_err(|e| e.to_string())?;
        }
        out.push(p);
    }
    Ok(out)
}

/// Loads all sources with up to `max_loads()` threads; results in source order.
fn load_all(srcs: &[PathBuf], cdir: &Path) -> Result<Vec<load::LinkData>, String> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<Result<load::LinkData, String>>>> =
        srcs.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..super::max_loads().min(srcs.len()) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(p) = srcs.get(i) else { break };
                let c = load::CacheUse {
                    dir: cdir.to_path_buf(),
                    rehash: false,
                };
                let cancel = Arc::new(AtomicBool::new(false));
                let r = load::load(p, "ifc", None, Some(&c), &cancel, &mut |_, _| {})
                    .map_err(|e| format!("{e:?}"));
                if let Ok(mut slot) = slots[i].lock() {
                    *slot = Some(r);
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|m| {
            m.into_inner()
                .ok()
                .flatten()
                .unwrap_or_else(|| Err("not loaded".into()))
        })
        .collect()
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

pub fn run(n: usize, e: usize, size: (u32, u32)) -> Result<(), String> {
    let root = std::env::temp_dir().join(format!("ddc-links-bench-n{n}-e{e}"));
    let srcs = corpus(&root, n, e)?;
    let cdir = cache::dir_for(&root.join("host.ddc"));
    let _ = std::fs::remove_dir_all(&cdir);
    let t = Instant::now();
    let cold = load_all(&srcs, &cdir)?;
    let cold_ms = ms(t);
    if cold.iter().any(|d| d.hit) {
        return Err("cold run hit a cache".into());
    }
    drop(cold);
    // host project and its link rows exist before the open is timed (the file is read)
    let mut ed = Editor::new_in_memory().map_err(|e| e.to_string())?;
    add_walls(&mut ed, 200, -200.0)?;
    let mut ids = vec![];
    for (k, p) in srcs.iter().enumerate() {
        let xf = Transform {
            dx: (k % 5) as f64 * 120.0,
            dy: (k / 5) as f64 * 250.0,
            ..Default::default()
        };
        let id = ed
            .model
            .add_link(&p.to_string_lossy(), "", xf)
            .map_err(|e| e.to_string())?;
        ids.push(id);
    }
    // the running app has its GPU pipelines; a host-only run warms them outside the timer
    let mut sc3 = crate::edit3d::scene3d::Cache::default();
    let (host, _) = crate::edit3d::scene3d::compose_with(&mut ed, &mut sc3);
    let w = ddc_render::synth::bench(&host, size, 1, ddc_render::Quality::Off)?;
    let gpu_warmup_ms = w.setup_ms + w.settle_ms;
    // warm open: cache reads (parallel), host scene, link groups, renderer, first frames
    let t = Instant::now();
    let warm = load_all(&srcs, &cdir)?;
    let warm_load_ms = ms(t);
    let hits = warm.iter().filter(|d| d.hit).count();
    let mut st = State::default();
    for (id, d) in ids.into_iter().zip(warm) {
        st.loaded.insert(id, Loaded::new(d));
    }
    let t1 = Instant::now();
    let mut sc3 = crate::edit3d::scene3d::Cache::default();
    let (host, _) = crate::edit3d::scene3d::compose_with(&mut ed, &mut sc3);
    let scene = super::attach(&st, &ed, host);
    let compose_ms = ms(t1);
    let cpu_ms = ms(t);
    let tris = scene.triangle_count()
        + scene
            .instanced
            .iter()
            .map(|g| g.inst.len() * g.proto.triangles.len())
            .sum::<usize>();
    let r = ddc_render::synth::bench(&scene, size, 8, ddc_render::Quality::Off)?;
    let first_ms = cpu_ms + r.setup_ms + r.settle_ms / f64::from(r.settle_frames.max(1));
    let settled_ms = cpu_ms + r.setup_ms + r.settle_ms;
    // plan section of one link on the UI thread (level change, first plan after a load)
    let plan_ms = st
        .loaded
        .values()
        .map(|ld| {
            let t = Instant::now();
            let lines = super::derive::plan_lines(&ld.data.elems, &Transform::default(), 1.2);
            std::hint::black_box(lines.len());
            ms(t)
        })
        .fold(0.0, f64::max);
    // host edits with the links loaded: scene rebuilt, link groups must be the same allocations
    let g0 = super::groups(&st, &ed);
    let (b0, p0) = (source::builds(), source::placements());
    let (mut edit, mut extra) = (vec![], vec![]);
    let ty = ed.current_type().ok_or("no wall type")?.id;
    for k in 0..5 {
        let y = -300.0 - k as f64 * 3.0;
        let w = WallSpec {
            a: (0.0, y),
            b: (1.5, y),
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ty,
            offset0: None,
        };
        let t = Instant::now();
        ed.run_op(crate::editor::OpFn(Arc::new(move |m| m.add_wall(&w).map(|_| ()))))
            .map_err(|e| e.to_string())?;
        let (host, _) = crate::edit3d::scene3d::compose_with(&mut ed, &mut sc3);
        let ta = Instant::now();
        let sc = super::attach(&st, &ed, host);
        extra.push(ms(ta));
        let tail = sc.instanced.len().saturating_sub(g0.len());
        let same = sc.instanced.len() >= g0.len()
            && g0
                .iter()
                .zip(&sc.instanced[tail..])
                .all(|(a, b)| Arc::ptr_eq(&a.proto, &b.proto) && Arc::ptr_eq(&a.inst, &b.inst));
        if !same {
            return Err("host edit replaced link groups".into());
        }
        edit.push(ms(t));
    }
    edit.sort_by(f64::total_cmp);
    extra.sort_by(f64::total_cmp);
    let rebuilds = source::builds() - b0 + (source::placements() - p0);
    let line = serde_json::json!({
        "date": crate::shell::recover_win::fmt_time(web_time::SystemTime::now()),
        "bench": "links",
        "adapter": r.adapter,
        "links": n,
        "walls_per_link": e,
        "cache_hits": hits,
        "triangles": tris,
        "cold_import_ms": cold_ms.round(),
        "warm_load_ms": warm_load_ms.round(),
        "warm_compose_ms": compose_ms.round(),
        "renderer_setup_ms": r.setup_ms.round(),
        "warm_first_frame_ms": first_ms.round(),
        "warm_settled_ms": settled_ms.round(),
        "host_edit_cpu_ms_median": edit[edit.len() / 2].round(),
        "host_edit_cpu_ms_max": edit[edit.len() - 1].round(),
        "host_edit_links_ms_max": (extra[extra.len() - 1] * 10.0).round() / 10.0,
        "gpu_warmup_ms": gpu_warmup_ms.round(),
        "link_rebuilds_on_host_edit": rebuilds,
        "plan_section_ms_max": (plan_ms * 10.0).round() / 10.0,
        "moving_p95_ms": (r.moving.p95_ms * 10.0).round() / 10.0,
    });
    println!("{line}");
    Ok(())
}
