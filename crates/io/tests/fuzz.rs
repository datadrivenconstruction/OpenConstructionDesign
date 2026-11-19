//! Mutation fuzz of the untrusted-input readers. Ignored: run through `tools/fuzz.ps1`
//! (or `DDC_FUZZ_SECS=10 cargo test --release -p ddc-io --test fuzz -- --ignored --nocapture`).
//! Seeds come from our own exporters; a failing input is saved to the temp dir.

#[path = "common/mutate.rs"]
mod mutate;

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::process::{Command, Stdio};

use ddc_bridge_proto::{decode, FrameReader};
use ddc_io::sheets::{compose, LengthUnits, SheetSpec, Viewport};
use ddc_io::{dxf, ifc, pdf, xlsx};
use ddc_model::{LayerSpec, Model, OpeningSpec, PlanViewSpec, RoomSpec, Swing, WallSpec};
use mutate::{mutate, run_target, Counting, Rng};

#[global_allocator]
static ALLOC: Counting = Counting;

fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type(
            "W",
            &[
                LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                },
                LayerSpec {
                    material: "Insulation".into(),
                    thickness: 0.1,
                },
            ],
        )
        .unwrap();
    let c = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
    let specs: Vec<WallSpec> = (0..4)
        .map(|k| WallSpec {
            a: c[k],
            b: c[(k + 1) % 4],
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: Some(0.0),
        })
        .collect();
    let ids = m.add_walls(&specs).unwrap();
    let op = |along, width, sill, height, kind: &str| OpeningSpec {
        along,
        width,
        sill,
        height,
        kind: kind.into(),
    };
    m.add_opening_with(ids[0], &op(1.0, 0.9, 0.0, 2.1, "door"), Swing::default())
        .unwrap();
    m.add_opening(ids[2], &op(2.0, 1.5, 0.9, 1.5, "window"))
        .unwrap();
    m.add_room(&RoomSpec {
        level: lv,
        name: "Room".into(),
        number: "101".into(),
        point: (3.0, 2.0),
        height: 3.0,
    })
    .unwrap();
    m
}

fn sheet() -> ddc_io::sheets::SheetDrawing {
    let m = model();
    let lv = 1; // first level of model()
    let view = m
        .plan_view(&PlanViewSpec {
            level: lv,
            cut_height: 1.2,
            grids: vec![],
        })
        .unwrap();
    compose(&SheetSpec {
        paper: "A3".into(),
        landscape: true,
        standard: "iso".into(),
        fields: BTreeMap::new(),
        viewports: vec![Viewport {
            view: &view,
            scale: 50.0,
            rect: None,
            title: "Plan".into(),
        }],
        units: LengthUnits::Metric,
        area_unit: None,
        logo: None,
    })
    .unwrap()
}

fn plain(r: &mut mutate::Rng, s: &[u8]) -> Vec<u8> {
    mutate(r, s)
}

#[test]
#[ignore]
fn fuzz_ifc_step() {
    let seed = ifc::export_ifc(&model()).unwrap().into_bytes();
    run_target("ifc_step", &[seed], plain, |b| {
        let _ = ifc::StepFile::parse(b);
    });
}

#[test]
#[ignore]
fn fuzz_ifc_import() {
    let seed = ifc::export_ifc(&model()).unwrap().into_bytes();
    run_target("ifc_import", &[seed], plain, |b| {
        let mut m = Model::open(":memory:").unwrap();
        let _ = ifc::import_ifc(&mut m, b);
    });
}

#[test]
#[ignore]
fn fuzz_dxf() {
    let seed = dxf::write_string(&sheet().to_dxf()).into_bytes();
    run_target("dxf", &[seed], plain, |b| {
        if let Ok(doc) = dxf::read_bytes(b) {
            let mut m = Model::open(":memory:").unwrap();
            let _ = dxf::import_into(&mut m, &doc);
        }
    });
}

#[test]
#[ignore]
fn fuzz_pdf_import() {
    let seed = sheet().to_pdf().unwrap();
    run_target("pdf", &[seed], plain, |b| {
        if let Ok(f) = pdf::import::PdfFile::parse(b) {
            f.set_decode_budget(8 << 20);
            for i in 0..f.page_count().min(3) {
                if let Ok(p) = f.page(i) {
                    let mut m = Model::open(":memory:").unwrap();
                    let _ = pdf::import::import_page(
                        &mut m,
                        &p,
                        &pdf::import::ImportOptions::default(),
                    );
                    let o = pdf::import::ImportOptions::default();
                    let _ = pdf::import::propose_walls(&p, &o, &Default::default());
                }
            }
        }
    });
}

/// Mutates either the zip container or one inner part (re-zipped), so the XML readers get reached.
fn xlsx_gen(r: &mut Rng, seed: &[u8]) -> Vec<u8> {
    if r.below(3) == 0 {
        return mutate(r, seed);
    }
    let mut zin = zip::ZipArchive::new(Cursor::new(seed)).unwrap();
    let pick = r.below(zin.len());
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..zin.len() {
        let mut f = zin.by_index(i).unwrap();
        let mut d = Vec::new();
        f.read_to_end(&mut d).unwrap();
        if i == pick {
            d = mutate(r, &d);
        }
        out.start_file(f.name().unwrap(), zip::write::SimpleFileOptions::default())
            .unwrap();
        out.write_all(&d).unwrap();
    }
    out.finish().unwrap().into_inner()
}

#[test]
#[ignore]
fn fuzz_xlsx_import() {
    let p = std::env::temp_dir().join(format!("ddc-fuzz-seed-{}.xlsx", std::process::id()));
    xlsx::export_xlsx(&model(), &p).unwrap();
    let seed = std::fs::read(&p).unwrap();
    let _ = std::fs::remove_file(&p);
    run_target("xlsx", &[seed], xlsx_gen, |b| {
        let p = std::env::temp_dir().join(format!(
            "ddc-fuzz-in-{}-{:?}.xlsx",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&p, b).unwrap();
        let mut m = model();
        let _ = xlsx::import_xlsx(&mut m, &p);
        let _ = std::fs::remove_file(&p);
    });
}

/// Full stream (preamble + frames) produced by the reference bridge.
fn bridge_stream() -> Vec<u8> {
    let input = std::env::temp_dir().join(format!("ddc-fuzz-bridge-in-{}.rvt", std::process::id()));
    std::fs::write(&input, b"x").unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_ddc-bridge-sample"))
        .args(["--protocol", "1", "--input"])
        .arg(&input)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = Vec::new();
    c.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    let _ = c.wait();
    let _ = std::fs::remove_file(&input);
    assert!(out.len() > 100, "reference bridge produced no stream");
    out
}

#[test]
#[ignore]
fn fuzz_bridge_stream() {
    run_target("bridge_stream", &[bridge_stream()], plain, |b| {
        let mut r = FrameReader::new(Cursor::new(b));
        while let Ok(Some(_)) = r.next_frame() {}
    });
}

#[test]
#[ignore]
fn fuzz_bridge_decode() {
    let s = bridge_stream();
    let (mut bodies, mut at) = (vec![], 6usize);
    while at + 4 <= s.len() {
        let n = u32::from_le_bytes(s[at..at + 4].try_into().unwrap()) as usize;
        if at + 4 + n > s.len() {
            break;
        }
        bodies.push(s[at + 4..at + 4 + n].to_vec());
        at += 4 + n;
    }
    assert!(!bodies.is_empty());
    run_target("bridge_decode", &bodies, plain, |b| {
        if let Ok(fr) = decode(b) {
            let _ = ddc_bridge_proto::encode(&fr);
        }
    });
}

/// Deterministic corpus: 500 truncated/mutated files built from our own exports
/// (IFC, DXF, PDF, xlsx, 125 each); every reader must return, never panic.
#[test]
fn corpus_500_mutations_never_panic() {
    let ifc_seed = ifc::export_ifc(&model()).unwrap().into_bytes();
    let dxf_seed = dxf::write_string(&sheet().to_dxf()).into_bytes();
    let pdf_seed = sheet().to_pdf().unwrap();
    let xp = std::env::temp_dir().join(format!("ddc-corpus-seed-{}.xlsx", std::process::id()));
    xlsx::export_xlsx(&model(), &xp).unwrap();
    let xlsx_seed = std::fs::read(&xp).unwrap();
    let _ = std::fs::remove_file(&xp);
    let mut r = Rng::new(0x5EED_0082);
    let mut panics = vec![];
    for k in 0..500usize {
        let (fmt, seed) = match k % 4 {
            0 => ("ifc", &ifc_seed),
            1 => ("dxf", &dxf_seed),
            2 => ("pdf", &pdf_seed),
            _ => ("xlsx", &xlsx_seed),
        };
        let inp = if k % 8 < 2 {
            seed[..r.below(seed.len())].to_vec()
        } else if fmt == "xlsx" {
            xlsx_gen(&mut r, seed)
        } else {
            mutate(&mut r, seed)
        };
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match fmt {
            "ifc" => {
                let mut m = Model::open(":memory:").unwrap();
                let _ = ifc::import_ifc(&mut m, &inp);
            }
            "dxf" => {
                if let Ok(doc) = dxf::read_bytes(&inp) {
                    let mut m = Model::open(":memory:").unwrap();
                    let _ = dxf::import_into(&mut m, &doc);
                }
            }
            "pdf" => {
                if let Ok(f) = pdf::import::PdfFile::parse(&inp) {
                    f.set_decode_budget(8 << 20);
                    for i in 0..f.page_count().min(3) {
                        if let Ok(p) = f.page(i) {
                            let mut m = Model::open(":memory:").unwrap();
                            let o = pdf::import::ImportOptions::default();
                            let _ = pdf::import::import_page(&mut m, &p, &o);
                        }
                    }
                }
            }
            _ => {
                let p = std::env::temp_dir()
                    .join(format!("ddc-corpus-{}-{k}.xlsx", std::process::id()));
                std::fs::write(&p, &inp).unwrap();
                let mut m = model();
                let _ = xlsx::import_xlsx(&mut m, &p);
                let _ = std::fs::remove_file(&p);
            }
        }))
        .is_ok();
        if !ok {
            let f = std::env::temp_dir().join(format!("ddc-corpus-fail-{fmt}-{k}.bin"));
            let _ = std::fs::write(&f, &inp);
            panics.push(format!("{fmt} #{k} saved to {}", f.display()));
        }
    }
    assert!(panics.is_empty(), "panics: {panics:#?}");
}
