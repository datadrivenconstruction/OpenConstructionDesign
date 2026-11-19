// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One corpus sample through the native importers; prints a single `CORPUS {json}` line.
//! Ignored: driven by `tools/corpus.ps1` with DDC_CORPUS_FILE=<path> (.ifc, .dxf, .pdf).
//! Sample files stay outside the repository.

use std::time::Instant;

use ddc_io::{dxf, ifc, pdf};
use ddc_model::Model;

fn json(counts: &[(&str, usize)], warnings: usize, secs: f64) -> String {
    let c: Vec<String> = counts.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    format!(
        "{{\"counts\":{{{}}},\"warnings\":{warnings},\"secs\":{secs:.3}}}",
        c.join(",")
    )
}

#[test]
#[ignore = "needs DDC_CORPUS_FILE"]
fn corpus_sample() {
    let Ok(file) = std::env::var("DDC_CORPUS_FILE") else {
        eprintln!("corpus: DDC_CORPUS_FILE not set, skipped");
        return;
    };
    let path = std::path::Path::new(&file);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut m = Model::open(":memory:").unwrap();
    let t = Instant::now();
    let line = match ext.as_str() {
        "ifc" => {
            let r = ifc::import_ifc_file(&mut m, path).unwrap_or_else(|e| panic!("{file}: {e:?}"));
            let skipped: usize = r.skipped.values().sum();
            json(
                &[
                    ("levels", r.levels),
                    ("wall_types", r.wall_types),
                    ("walls", r.walls),
                    ("openings", r.openings),
                    ("slabs", r.slabs),
                    ("rooms", r.rooms),
                    ("separators", r.separators),
                    ("space_boundaries", r.space_boundaries),
                ],
                skipped,
                t.elapsed().as_secs_f64(),
            )
        }
        "dxf" => {
            let doc = dxf::read_file(path).unwrap_or_else(|e| panic!("{file}: {e:?}"));
            let r = dxf::import_into(&mut m, &doc).unwrap_or_else(|e| panic!("{file}: {e:?}"));
            json(
                &[
                    ("layers", r.layers),
                    ("blocks", r.blocks),
                    ("lines", r.lines),
                    ("polylines", r.polylines),
                    ("arcs", r.arcs),
                    ("circles", r.circles),
                    ("texts", r.texts),
                    ("inserts", r.inserts),
                    ("attributes", r.attributes),
                ],
                r.skipped_unknown + r.skipped_inserts,
                t.elapsed().as_secs_f64(),
            )
        }
        "pdf" => {
            let data = std::fs::read(path).unwrap();
            let opt = pdf::import::ImportOptions::default();
            let f = pdf::import::PdfFile::parse(&data).unwrap_or_else(|e| panic!("{file}: {e:?}"));
            let pages = f.page_count();
            let r = pdf::import::import_file(&mut m, &data, 0, &opt)
                .unwrap_or_else(|e| panic!("{file}: {e:?}"));
            json(
                &[
                    ("pages", pages),
                    ("layers", r.layers),
                    ("lines", r.lines),
                    ("polylines", r.polylines),
                    ("arcs", r.arcs),
                    ("texts", r.texts),
                ],
                r.truncated as usize,
                t.elapsed().as_secs_f64(),
            )
        }
        other => panic!("{file}: unsupported extension .{other}"),
    };
    println!("CORPUS {line}");
}
