//! Imports run as one model step: one undo takes the whole import back, redo restores it, and an
//! import failing part-way leaves the model content (hash) and the undo stack as they were.
use super::*;
use ddc_io::bridge::proto::{self, Curve, Element, Frame, Top};
use std::sync::Arc;

fn hash(e: &Editor) -> u64 {
    e.model.content_hash().unwrap()
}

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ddc_pro1_{}_{name}", std::process::id()))
}

/// Rows of the tables imports write (a redo may stamp a new import time, so not the hash).
fn rows(e: &Editor) -> Vec<i64> {
    ["level", "wall", "wall_type", "d_layer", "d_line"]
        .iter()
        .map(|t| {
            e.model
                .sql()
                .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
                .unwrap()
        })
        .collect()
}

/// One undo step for the action, undo back to `before` exactly, redo back to the imported rows.
fn undo_redo_exact(e: &mut Editor, before: u64, u: usize) {
    let (after, made) = (hash(e), rows(e));
    assert_ne!(after, before, "the import changed the model");
    assert_eq!(e.undo.len(), u + 1, "one import, one undo step");
    e.undo();
    assert_eq!(hash(e), before, "one undo takes the whole import back");
    e.redo();
    assert_eq!(rows(e), made, "redo restores the import");
    assert_eq!(e.undo.len(), u + 1);
    e.undo();
    assert_eq!(hash(e), before, "undo after redo is exact again");
    e.redo();
}

/// A refused import: same content, same undo stack, no journal record left.
fn unchanged(e: &Editor, before: u64, u: usize, top: i64) {
    assert_eq!(hash(e), before, "a failed import leaves the model identical");
    assert_eq!(e.undo.len(), u, "a failed import pushes no undo step");
    assert_eq!(e.model.op_top().unwrap(), top, "no journal record left");
    assert!(e.model.sql().is_autocommit(), "no transaction left open");
}

fn ifc_file(name: &str) -> std::path::PathBuf {
    let mut src = Editor::new_in_memory().unwrap();
    src.commit_walls(vec![
        (v2(0.0, 0.0), v2(5.0, 0.0)),
        (v2(5.0, 0.0), v2(5.0, 4.0)),
    ]);
    let p = tmp(name);
    ddc_io::ifc::write_ifc_file(&src.model, &p).unwrap();
    p
}

fn ifc_op(p: &std::path::Path, fail: bool) -> OpFn {
    let p = p.to_path_buf();
    OpFn(Arc::new(move |m: &mut Model| {
        ddc_io::ifc::import_ifc_path(m, &p)
            .map_err(|e| ddc_model::Error::Invalid(e.to_string()))?;
        if fail {
            return Err(ddc_model::Error::Invalid("injected after the import".into()));
        }
        Ok(())
    }))
}

#[test]
fn ifc_import_is_one_step_and_a_late_failure_changes_nothing() {
    let p = ifc_file("a.ifc");
    let mut e = Editor::new_in_memory().unwrap();
    e.commit_walls(vec![(v2(0.0, 10.0), v2(3.0, 10.0))]);
    let (before, u, top) = (hash(&e), e.undo.len(), e.model.op_top().unwrap());
    assert!(e.run_op(ifc_op(&p, true)).is_err());
    unchanged(&e, before, u, top);
    e.run_op(ifc_op(&p, false)).unwrap();
    undo_redo_exact(&mut e, before, u);
    let _ = std::fs::remove_file(&p);
}

fn dxf_doc() -> Arc<ddc_io::dxf::DxfDocument> {
    use ddc_io::dxf::{Common, DxfDocument, Entity, Line};
    let entities = (0..20)
        .map(|k| {
            let y = k as f64;
            Entity::Line(Line {
                common: Common::on_layer("A"),
                start: [0.0, y, 0.0],
                end: [4.0, y, 0.0],
            })
        })
        .collect();
    Arc::new(DxfDocument {
        entities,
        ..Default::default()
    })
}

fn dxf_op(doc: Arc<ddc_io::dxf::DxfDocument>, fail: bool) -> OpFn {
    OpFn(Arc::new(move |m: &mut Model| {
        ddc_io::dxf::import_into(m, &doc).map_err(|e| ddc_model::Error::Invalid(e.to_string()))?;
        if fail {
            return Err(ddc_model::Error::Invalid("injected after the import".into()));
        }
        Ok(())
    }))
}

#[test]
fn drawing_import_is_one_step_and_a_late_failure_changes_nothing() {
    let mut e = Editor::new_in_memory().unwrap();
    let (before, u, top) = (hash(&e), e.undo.len(), e.model.op_top().unwrap());
    assert!(e.run_op(dxf_op(dxf_doc(), true)).is_err());
    unchanged(&e, before, u, top);
    e.run_op(dxf_op(dxf_doc(), false)).unwrap();
    undo_redo_exact(&mut e, before, u);
}

fn bridge_frames(end: bool) -> Vec<Frame> {
    let mut f = vec![
        Frame::Hello(proto::Hello {
            protocol: proto::PROTOCOL,
            bridge: "b".into(),
            version: "1".into(),
            formats: vec!["rvt".into()],
            capabilities: vec![],
        }),
        Frame::Units(proto::Units { length_to_m: 1.0 }),
        Frame::Levels(vec![proto::Level {
            guid: "L1".into(),
            name: "L1".into(),
            elevation: 0.0,
        }]),
        Frame::Types(vec![proto::Type::Wall {
            guid: "T1".into(),
            name: "T".into(),
            layers: vec![proto::Layer {
                material: "m".into(),
                thickness: 0.2,
            }],
        }]),
        Frame::Elements(vec![Element::Wall {
            guid: "W1".into(),
            level: "L1".into(),
            wall_type: "T1".into(),
            axis: Curve::Line {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
            },
            base_offset: 0.0,
            top: Top::Unconnected { height: 3.0 },
            offset0: None,
        }]),
    ];
    if end {
        f.push(Frame::End(proto::Stats {
            elements: 1,
            warnings: 0,
        }));
    }
    f
}

#[test]
fn module_import_is_one_step_and_a_cut_stream_changes_nothing() {
    let p = tmp("b.rvt");
    std::fs::write(&p, b"x").unwrap();
    let mut e = Editor::new_in_memory().unwrap();
    let (before, u, top) = (hash(&e), e.undo.len(), e.model.op_top().unwrap());
    // levels, types and the wall are read before the stream turns out to have no End
    assert!(e.apply_import(&p, bridge_frames(false)).is_err());
    unchanged(&e, before, u, top);
    e.apply_import(&p, bridge_frames(true)).unwrap();
    undo_redo_exact(&mut e, before, u);
    let _ = std::fs::remove_file(&p);
}

fn add_level_ok(m: &mut Model, _id: i64, v: f64) -> Result<(), ddc_model::Error> {
    m.add_level("XL", v).map(|_| ())
}

fn refuse(_m: &mut Model, _id: i64, _v: f64) -> Result<(), ddc_model::Error> {
    Err(ddc_model::Error::Invalid("refused cell".into()))
}

type SetFn = fn(&mut Model, i64, f64) -> Result<(), ddc_model::Error>;

fn prop(set: SetFn) -> ddc_io::xlsx::PropEdit {
    ddc_io::xlsx::PropEdit {
        sheet: "Walls".into(),
        table: "wall",
        id: 1,
        key: "Height",
        value: 7.0,
        set,
        delete: false,
    }
}

fn new_level(name: &str, elevation: f64) -> ddc_model::Edit {
    ddc_model::Edit::NewLevel {
        name: name.into(),
        elevation,
    }
}

#[test]
fn xlsx_import_is_one_step_and_a_refused_cell_changes_nothing() {
    let mut e = Editor::new_in_memory().unwrap();
    let (before, u, top) = (hash(&e), e.undo.len(), e.model.op_top().unwrap());
    // the book edits and one cell are written before a later cell is refused
    let r = e.apply_xlsx_props(
        vec![new_level("B1", 3.0)],
        vec![prop(add_level_ok), prop(refuse)],
        "book",
    );
    assert!(r.is_err());
    unchanged(&e, before, u, top);
    e.apply_xlsx_props(vec![new_level("B1", 3.0)], vec![prop(add_level_ok)], "book")
        .unwrap();
    undo_redo_exact(&mut e, before, u);
    // edits alone: the plain path, one record too
    let u = e.undo.len();
    let mid = hash(&e);
    e.apply_xlsx(vec![new_level("B2", 6.0), new_level("B3", 9.0)], "book")
        .unwrap();
    undo_redo_exact(&mut e, mid, u);
}
