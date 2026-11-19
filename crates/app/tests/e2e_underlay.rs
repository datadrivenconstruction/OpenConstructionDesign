// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Underlay workflow: .dxf/.dwg file -> underlay -> recognition -> accept -> wall edits ->
//! undo everything -> every table, the journal included, equals the start dump.
//! The light test runs on the built-in sample plan; the corpus tests are `--ignored`.

mod e2e_common;

use std::path::Path;

use ddc_app::drawing::{read_drawing, Class, Drawings, Origin};
use ddc_app::editor::{Editor, OpFn};
use ddc_geom::v2;
use ddc_io::dxf::DxfDocument;
use ddc_model::Model;
use std::sync::Arc;

/// Drawing -> recognition -> accept (one op) -> edits -> undo all. Returns the number of
/// walls accepted (0 when the drawing yields no wall proposals).
fn chain(doc: &DxfDocument, name: &str) -> usize {
    let mut ed = Editor::new_in_memory().unwrap();
    let start = e2e_common::dump(&ed.model);
    e2e_common::assert_clean(&ed.model, "start");

    // the underlay lives in the session, not in the model
    let mut dr = Drawings::default();
    let summary = dr.add(
        doc,
        name,
        None,
        ed.level,
        None,
        Origin::MinToZero,
        v2(0.0, 0.0),
    );
    eprintln!("underlay: {summary}");
    assert_eq!(dr.list.len(), 1, "{name}: drawing placed");
    e2e_common::assert_same(&e2e_common::dump(&ed.model), &start, "after underlay add");

    let msg = dr.recognize(&ed);
    eprintln!("{msg}");
    let rc = dr.recog.as_mut().expect("recognition result");
    rc.set_class(Class::Walls, true);
    let proposed = rc.ids(Class::Walls).len();
    if proposed == 0 {
        eprintln!("{name}: no wall proposals, accept skipped");
        e2e_common::assert_same(&e2e_common::dump(&ed.model), &start, "after recognize");
        return 0;
    }

    dr.accept(&mut ed)
        .unwrap_or_else(|e| panic!("{name}: accept: {e}"));
    e2e_common::assert_clean(&ed.model, "after accept");
    let walls: Vec<i64> = ed.walls.iter().map(|w| w.id).collect();
    assert!(!walls.is_empty(), "{name}: accepted walls are on the level");
    assert!(ed.can_undo(), "{name}: accept is an undo step");
    let accepted = e2e_common::dump(&ed.model);
    assert_ne!(e2e_common::hash_of(&accepted), e2e_common::hash_of(&start));

    // edit: type + height of the first wall, new axis end of the last one
    let first = ed.walls[0].clone();
    let other = ed
        .types
        .iter()
        .map(|t| t.id)
        .find(|t| *t != first.wall_type);
    let taller = ed.height + 0.5;
    ed.edit_wall(first.id, None, other, Some(taller));
    e2e_common::assert_clean(&ed.model, "after edit_wall");

    let last = ed.walls[ed.walls.len() - 1].clone();
    let (a, b) = ((last.a.x, last.a.y), (last.b.x + 0.25, last.b.y + 0.25));
    let id = last.id;
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.set_wall_axis(id, a, b)
    })))
    .unwrap_or_else(|e| panic!("{name}: set_wall_axis: {e}"));
    e2e_common::assert_clean(&ed.model, "after axis edit");
    assert_ne!(
        e2e_common::hash_of(&e2e_common::dump(&ed.model)),
        e2e_common::hash_of(&accepted)
    );

    let mut guard = 0;
    while ed.can_undo() {
        ed.undo();
        e2e_common::assert_clean(&ed.model, "during undo");
        guard += 1;
        assert!(guard < 1000, "{name}: undo does not drain");
    }
    e2e_common::assert_same(&e2e_common::dump(&ed.model), &start, "after undo all");
    assert!(
        ed.walls.is_empty(),
        "{name}: editor reloaded to the empty model"
    );
    walls.len()
}

#[test]
fn e2e_underlay_dxf_recognize_accept_edit_undo_all() {
    let doc = ddc_io::drawing_model::fixture::sample_plan(0);
    // through a real file: the same read path as the import dialog
    let path = e2e_common::temp_file("underlay_sample.dxf");
    ddc_io::dxf::write_file(&doc, &path).unwrap();
    let doc = read_drawing(&path).unwrap_or_else(|e| panic!("read_drawing: {e}"));
    let _ = std::fs::remove_file(&path);

    let n = chain(&doc, "sample_plan");
    assert!(n >= 4, "sample plan gives walls, got {n}");
}

#[test]
fn e2e_underlay_dwg_without_reader_is_refused() {
    if ddc_io::dxf::DWG_SUPPORTED {
        return;
    }
    let path = e2e_common::temp_file("underlay_fake.dwg");
    std::fs::write(&path, b"AC1032\0\0\0\0\0\0garbage").unwrap();
    let r = read_drawing(&path);
    let _ = std::fs::remove_file(&path);
    assert_eq!(r.err(), Some(ddc_app::drawing::dwg_unavailable()));
}

fn heavy(var: &str, default: &str) {
    let Some(path) = e2e_common::corpus(var, default) else {
        return;
    };
    run_file(&path);
}

fn run_file(path: &Path) {
    let is_dwg = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("dwg"));
    if is_dwg && !ddc_io::dxf::DWG_SUPPORTED {
        eprintln!("e2e: native .dwg reader not in this build, skipped");
        return;
    }
    let doc = read_drawing(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let n = chain(&doc, &path.display().to_string());
    eprintln!("{}: {n} walls accepted and undone", path.display());
}

#[test]
#[ignore = "corpus"]
fn e2e_underlay_corpus_dwg() {
    heavy(
        "DDC_E2E_DWG",
        "",
    );
}

// CORPUS_90 lists no .dxf sample: DDC_E2E_DXF must be set to run this one
#[test]
#[ignore = "corpus"]
fn e2e_underlay_corpus_dxf() {
    heavy("DDC_E2E_DXF", "");
}
