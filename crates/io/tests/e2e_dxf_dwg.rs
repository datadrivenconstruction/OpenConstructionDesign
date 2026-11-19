// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! DXF/DWG at io level: write -> read keeps the entity count, import into a model is one
//! undo step that returns every table (journal included) to the empty start.
//! Corpus files (`--ignored`): env `DDC_E2E_DWG` / `DDC_E2E_DXF`.

#[path = "../../app/tests/e2e_common/mod.rs"]
mod e2e_common;

use ddc_io::dxf::{self, Common, DxfDocument, Entity, Layer, Line};
use ddc_model::Model;

fn lines_doc(n: usize) -> DxfDocument {
    let mut d = DxfDocument::default();
    d.layers.push(Layer {
        name: "WALLS".into(),
        flags: 0,
        color: 7,
        linetype: "CONTINUOUS".into(),
        extra: vec![],
    });
    for i in 0..n {
        let y = i as f64 * 250.0;
        d.entities.push(Entity::Line(Line {
            common: Common::on_layer("WALLS"),
            start: [0.0, y, 0.0],
            end: [4000.0 + y, y, 0.0],
        }));
    }
    d
}

/// Import as one op, undo it, the model equals the empty start; returns the import report.
fn import_and_undo(doc: &DxfDocument, ctx: &str) -> dxf::ImportReport {
    let mut m = Model::open(":memory:").unwrap();
    let start = e2e_common::dump(&m);
    let j0 = e2e_common::journal_len(&m);
    let rep = dxf::import_into(&mut m, doc).unwrap_or_else(|e| panic!("{ctx}: import: {e:?}"));
    e2e_common::assert_clean(&m, &format!("{ctx}: after import"));
    assert_eq!(
        e2e_common::count(&m, "d_line"),
        rep.lines as i64,
        "{ctx}: d_line rows = report"
    );
    assert_eq!(
        e2e_common::journal_len(&m),
        j0 + 1,
        "{ctx}: import is one journal op"
    );
    m.undo().unwrap();
    e2e_common::assert_clean(&m, &format!("{ctx}: after undo"));
    e2e_common::assert_same(&e2e_common::dump(&m), &start, &format!("{ctx}: after undo"));
    rep
}

#[test]
fn e2e_dxf_round_trip_keeps_entity_count() {
    for doc in [
        lines_doc(40),
        ddc_io::drawing_model::fixture::sample_plan(0),
    ] {
        let text = dxf::write_string(&doc);
        let back = dxf::read_str(&text).unwrap();
        assert_eq!(
            back.entities.len(),
            doc.entities.len(),
            "entities after write+read"
        );
        assert_eq!(
            back.blocks.len(),
            doc.blocks.len(),
            "blocks after write+read"
        );
        // second pass is stable too
        let again = dxf::read_str(&dxf::write_string(&back)).unwrap();
        assert_eq!(again.entities.len(), back.entities.len());
    }
}

#[test]
fn e2e_dxf_file_round_trip() {
    let doc = lines_doc(25);
    let p = e2e_common::temp_file("e2e_lines.dxf");
    dxf::write_file(&doc, &p).unwrap();
    assert!(e2e_common::exists(&p));
    let back = dxf::read_file(&p).unwrap();
    let _ = std::fs::remove_file(&p);
    assert_eq!(back.entities.len(), 25);
}

#[test]
fn e2e_dxf_import_is_one_undo_step() {
    let rep = import_and_undo(&lines_doc(30), "lines");
    assert_eq!(rep.lines, 30);
    let rep = import_and_undo(
        &ddc_io::drawing_model::fixture::sample_plan(0),
        "sample_plan",
    );
    assert!(rep.lines > 0);
}

#[test]
fn e2e_dxf_import_twice_then_undo_twice() {
    let mut m = Model::open(":memory:").unwrap();
    let start = e2e_common::dump(&m);
    let doc = lines_doc(10);
    dxf::import_into(&mut m, &doc).unwrap();
    dxf::import_into(&mut m, &doc).unwrap();
    assert_eq!(e2e_common::count(&m, "d_line"), 20);
    m.undo().unwrap();
    m.undo().unwrap();
    e2e_common::assert_clean(&m, "after double undo");
    e2e_common::assert_same(&e2e_common::dump(&m), &start, "after double undo");
}

fn corpus_read_import(path: &std::path::Path) {
    let is_dwg = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("dwg"));
    if is_dwg && !dxf::DWG_SUPPORTED {
        eprintln!("e2e: native .dwg reader not in this build, skipped");
        return;
    }
    let doc = dxf::read_file(path).unwrap_or_else(|e| panic!("{}: read: {e:?}", path.display()));
    let n = doc.entities.len() + doc.blocks.iter().map(|b| b.entities.len()).sum::<usize>();
    assert!(n > 0, "{}: no entities read", path.display());
    let rep = import_and_undo(&doc, &path.display().to_string());
    eprintln!(
        "{}: entities {n}, imported lines {}",
        path.display(),
        rep.lines
    );
}

#[test]
#[ignore = "corpus"]
fn e2e_dwg_corpus_read_import_undo() {
    let Some(p) = e2e_common::corpus(
        "DDC_E2E_DWG",
        "",
    ) else {
        return;
    };
    corpus_read_import(&p);
}

// CORPUS_90 lists no .dxf sample: DDC_E2E_DXF must be set to run this one
#[test]
#[ignore = "corpus"]
fn e2e_dxf_corpus_read_import_undo() {
    let Some(p) = e2e_common::corpus("DDC_E2E_DXF", "") else {
        return;
    };
    corpus_read_import(&p);
}
