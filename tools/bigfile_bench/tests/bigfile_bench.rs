//! Big-file timings over the local corpus (ignored by default; files live outside the repo):
//! `cargo test -p bigfile-bench --release -- --ignored --nocapture`.
//! `DDC_BIG_N` files per format (3), `DDC_BIG_FILE` one file, `DDC_BENCH_DDC` model files
//! (`;`-separated, copied before the cache build), `DDC_BENCH_LEDGER` appends JSON lines.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bigfile_bench::{biggest, corpus_file, ledger_line, parse, Entry};
use ddc_io::ifc::{commit_plan, plan_file, StepFile, StreamOptions};
use ddc_io::job::Progress;
use ddc_model::Model;

fn n() -> usize {
    std::env::var("DDC_BIG_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3)
}

fn corpus() -> Vec<Entry> {
    match corpus_file().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(md) => parse(&md),
        None => {
            println!("skip: no corpus list (set DDC_CORPUS or add tools/bigfile_bench/corpus.md)");
            vec![]
        }
    }
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc-bigbench-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{tag}.ddc"))
}

fn drop_model(p: &Path) {
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", p.display()));
    }
}

fn ledger(name: &str, fields: &[(&str, f64)]) {
    let line = ledger_line(name, fields);
    println!("{line}");
    if let Ok(p) = std::env::var("DDC_BENCH_LEDGER") {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn mesh_cache(name: &str, m: &Model) {
    let t = Instant::now();
    let st = m.mesh_cache_build(&mut |_, _| true).unwrap();
    let build = t.elapsed().as_secs_f64();
    let (meshes, protos, tw, tp) = m.mesh_cache_totals().unwrap();
    let t = Instant::now();
    let pl = m.mesh_placements(None).unwrap();
    let mut coarse = 0u64;
    let mut seen = std::collections::HashSet::new();
    for p in &pl {
        if seen.insert(p.proto) {
            coarse += m.mesh_proto(p.proto, 2).unwrap().triangles.len() as u64;
        }
    }
    let load = t.elapsed().as_secs_f64();
    ledger(
        name,
        &[
            ("cache_s", build),
            ("meshes", meshes as f64),
            ("protos", protos as f64),
            ("reused", st.reused as f64),
            ("tris_world", tw as f64),
            ("tris_protos", tp as f64),
            ("tris_coarsest", coarse as f64),
            ("load_all_s", load),
        ],
    );
}

#[test]
#[ignore = "local corpus: DDC_CORPUS"]
fn ifc_stream_open_plan_commit() {
    for e in biggest(&corpus(), "ifc", n()) {
        let name = e.path.display().to_string();
        let mb = e.kb as f64 / 1024.0;
        let t = Instant::now();
        let f = match StepFile::open_with(&e.path, &StreamOptions::default(), None) {
            Ok(f) => f,
            Err(err) => {
                println!("{name}: open failed: {err}");
                continue;
            }
        };
        let open = t.elapsed().as_secs_f64();
        let ents = f.len();
        drop(f);
        let ctl = Progress::new();
        let t = Instant::now();
        let pl = match plan_file(&e.path, &ctl) {
            Ok(p) => p,
            Err(err) => {
                println!("{name}: plan failed: {err}");
                continue;
            }
        };
        let plan = t.elapsed().as_secs_f64();
        let db = scratch("ifc");
        drop_model(&db);
        let mut m = Model::open(&db.to_string_lossy()).unwrap();
        let t = Instant::now();
        let ok = commit_plan(&mut m, &pl).is_ok();
        let commit = t.elapsed().as_secs_f64();
        ledger(
            &name,
            &[
                ("mb", mb),
                ("entities", ents as f64),
                ("index_s", open),
                ("index_mb_s", mb / open.max(1e-9)),
                ("plan_s", plan),
                ("commit_s", commit),
                ("commit_ok", ok as u8 as f64),
                ("walls", pl.walls.len() as f64),
            ],
        );
        drop(m);
        drop_model(&db);
    }
}

#[test]
#[ignore = "local corpus: DDC_CORPUS"]
fn ifc_cancel_is_prompt_on_the_biggest_file() {
    let Some(e) = biggest(&corpus(), "ifc", 1).pop() else {
        return;
    };
    let ctl = Progress::new();
    let c = ctl.clone();
    let t0 = Instant::now();
    let h = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        c.cancel();
        Instant::now()
    });
    let r = plan_file(&e.path, &ctl);
    let end = Instant::now();
    let asked = h.join().unwrap();
    let lag = end.saturating_duration_since(asked).as_secs_f64();
    ledger(
        &e.path.display().to_string(),
        &[
            ("cancel_lag_s", lag),
            ("ran_s", t0.elapsed().as_secs_f64()),
            ("cancelled", r.is_err() as u8 as f64),
        ],
    );
    assert!(lag < 2.0, "cancel took {lag:.2} s");
}

#[test]
#[ignore = "local models: DDC_BENCH_DDC"]
fn mesh_cache_on_models() {
    let Ok(list) = std::env::var("DDC_BENCH_DDC") else {
        println!("skip: set DDC_BENCH_DDC to model files");
        return;
    };
    for (k, src) in list
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        let db = scratch(&format!("ddc{k}"));
        drop_model(&db);
        // a copy: the cache build writes to the file
        if std::fs::copy(src, &db).is_err() {
            println!("{src}: copy failed");
            continue;
        }
        let t = Instant::now();
        let m = Model::open(&db.to_string_lossy()).unwrap();
        ledger(src, &[("open_s", t.elapsed().as_secs_f64())]);
        mesh_cache(src, &m);
        drop(m);
        drop_model(&db);
    }
}

#[test]
#[ignore = "local corpus and an out-of-process import bridge"]
fn rvt_import_and_mesh_cache() {
    use ddc_io::bridge;
    let Some(dir) = bridge::default_dir() else {
        println!("skip: no module folder");
        return;
    };
    let mut trust = bridge::TrustStore::official();
    if let Some(p) = bridge::developer_keys_path() {
        trust.add_developer(bridge::load_developer_keys(&p, Some(dir.as_path())));
    }
    let Some(b) = bridge::discover(&dir, &trust)
        .into_iter()
        .find(|b| b.trust.is_ok() && b.formats.iter().any(|f| f == "rvt"))
    else {
        println!("skip: no trusted .rvt module installed");
        return;
    };
    for e in biggest(&corpus(), "rvt", n()) {
        let name = e.path.display().to_string();
        let t = Instant::now();
        let o = bridge::RunOptions::default();
        let frames = match bridge::collect_trusted(&b.dir, &trust, &e.path, &o, &mut |_| {}) {
            Ok(f) => f,
            Err(err) => {
                println!("{name}: module failed: {err}");
                continue;
            }
        };
        let read = t.elapsed().as_secs_f64();
        let db = scratch("rvt");
        drop_model(&db);
        let mut m = Model::open(&db.to_string_lossy()).unwrap();
        let t = Instant::now();
        let ok = bridge::import_frames(&mut m, &e.path, frames).is_ok();
        ledger(
            &name,
            &[
                ("mb", e.kb as f64 / 1024.0),
                ("module_s", read),
                ("import_s", t.elapsed().as_secs_f64()),
                ("ok", ok as u8 as f64),
            ],
        );
        mesh_cache(&name, &m);
        drop(m);
        drop_model(&db);
    }
}
