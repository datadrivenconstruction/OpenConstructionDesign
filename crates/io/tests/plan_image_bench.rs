//! Plan from a picture (g9) on our own sample floors: each generated layout is drawn as a
//! picture (filled walls, outlined walls, scale from a known width) and as a vector PDF page,
//! read back and scored against the layout. Gate: on these clean drawings at least 95 % of the
//! wall length (length-weighted IoU of the axes, within 20 mm) and 95 % of the openings match.
//! One `BENCH {...}` line per read; with `DDC_BENCH_RECORD=1` it is appended to
//! the file named by `DDC_BENCH_OUT` (default `target/ai_geometry.jsonl`).

use ddc_gen::{registry, GenOut};
use ddc_io::drawing_model::{propose, Memory, Underlay};
use ddc_io::pdf::export_pdf;
use ddc_io::pdf::import::{ImportOptions, PdfFile, Scale};
use ddc_io::plan_image::synth::{score, Score, SynthOpening, SynthPlan, SynthWall, WallStyle};
use ddc_io::plan_image::{read_plan, vector_options, ImageScale};
use serde_json::{json, Value as J};
use std::io::Write;
use std::time::Instant;

const GATE: f64 = 0.95;
/// Positions within 20 mm (plan g9).
const TOL: f64 = 0.02;

fn rooms() -> J {
    json!([
        {"name": "Hall", "area": 40.0},
        {"name": "Office 1", "area": 18.5},
        {"name": "Office 2", "area": 18.5},
        {"name": "Store", "area": 6.0, "window": false},
        {"name": "Meeting", "area": 24.0},
        {"name": "WC", "area": 3.0, "window": false},
    ])
}

/// The straight walls of a generated layout and the openings in them.
fn sample(id: &str, params: J, seed: u64) -> SynthPlan {
    let g = registry().iter().find(|g| g.id() == id).unwrap();
    let GenOut::Layout(l) = g.build(&params, seed).unwrap() else {
        panic!("{id} is not a layout");
    };
    let mut keep = vec![None; l.walls.len()];
    let mut walls = vec![];
    for (i, w) in l.walls.iter().enumerate() {
        if w.bulge == 0.0 {
            keep[i] = Some(walls.len());
            walls.push(SynthWall { a: w.a, b: w.b, thickness: w.role.thickness() });
        }
    }
    let openings = l
        .openings
        .iter()
        .filter_map(|o| {
            Some(SynthOpening { wall: keep[o.wall]?, along: o.along, width: o.width, door: o.door })
        })
        .collect();
    SynthPlan { walls, openings }
}

fn record(line: &J) {
    println!("BENCH {line}");
    if std::env::var_os("DDC_BENCH_RECORD").is_none() {
        return;
    }
    let path = std::env::var_os("DDC_BENCH_OUT").map_or_else(
        || std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ai_geometry.jsonl"),
        std::path::PathBuf::from,
    );
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

fn ts() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn r4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

fn line(name: &str, render: &str, s: &Score, ms: u128, px_per_m: f64, walls: usize, truth: &SynthPlan) -> J {
    json!({
        "ts": ts(),
        "scenario": format!("plan_image:{name}:{render}"),
        "status": "ok",
        "variant": std::env::var("DDC_BENCH_VARIANT").unwrap_or_else(|_| "current".into()),
        "wall_iou": r4(s.wall_iou),
        "wall_recall": r4(s.wall_recall),
        "wall_precision": r4(s.wall_precision),
        "openings": s.openings,
        "openings_found": s.openings_found,
        "openings_extra": s.openings_extra,
        "walls_true": truth.walls.len(),
        "walls_read": walls,
        "px_per_m": r4(px_per_m),
        "read_ms": ms,
    })
}

fn check(name: &str, render: &str, s: &Score) {
    assert!(s.wall_iou >= GATE, "{name} {render}: wall match {s:?}");
    assert!(s.opening_rate() >= GATE, "{name} {render}: openings {s:?}");
}

fn bench(name: &str, p: &SynthPlan) {
    for (render, k, style, known) in [
        ("png_filled", 50.0, WallStyle::Filled, false),
        ("png_outline", 60.0, WallStyle::Outline, false),
        ("png_known_width", 50.0, WallStyle::Filled, true),
    ] {
        let (img, shift) = p.render(k, 1.0, style);
        let scale = if known {
            let (lo, hi) = p.bounds();
            ImageScale::Width(hi.0 - lo.0)
        } else {
            ImageScale::PxPerM(k)
        };
        let t = Instant::now();
        let r = read_plan(&img, &scale, &[], None).unwrap();
        let ms = t.elapsed().as_millis();
        // a known width fixes the scale to a pixel over the whole plan
        let tol = if known { TOL + 1.0 / k } else { TOL };
        let s = score(p, shift, &r.proposals.walls, &r.proposals.openings, tol, 1.5 / k);
        record(&line(name, render, &s, ms, r.px_per_m, r.proposals.walls.len(), p));
        check(name, render, &s);
    }
    let (sheet, shift) = p.sheet(100.0, 20.0);
    let bytes = export_pdf(&[sheet]).unwrap();
    let t = Instant::now();
    let page = PdfFile::parse(&bytes).unwrap().page(0).unwrap();
    let opt = ImportOptions { scale: Scale::Ratio(100.0), ..Default::default() };
    let u = Underlay::from_pdf(&page, &opt).unwrap();
    let prop = propose(&u, &[], &vector_options(), &Memory::default());
    let ms = t.elapsed().as_millis();
    let s = score(p, shift, &prop.walls, &prop.openings, TOL, TOL);
    record(&line(name, "pdf_vector", &s, ms, 0.0, prop.walls.len(), p));
    check(name, "pdf_vector", &s);
}

#[test]
fn typical_floor_reads_back_from_a_picture_and_a_pdf() {
    for seed in [1, 2] {
        let p = sample("typical_floor", json!({"depth": 18.0}), seed);
        assert!(p.walls.len() >= 10 && !p.openings.is_empty());
        bench(&format!("typical_floor_{seed}"), &p);
    }
}

#[test]
fn area_program_reads_back_from_a_picture_and_a_pdf() {
    let p = sample("area_program", json!({"rooms": rooms()}), 1);
    assert!(p.walls.len() >= 6 && !p.openings.is_empty());
    bench("area_program_1", &p);
}
