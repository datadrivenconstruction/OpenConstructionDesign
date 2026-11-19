//! "Add to this project": the worker hands over shared frames, the UI thread applies them as
//! one operation.
use super::*;
use ddc_io::bridge::proto::{self, Curve, Element, Frame, Top};
use std::sync::Arc;

fn frames() -> Vec<Frame> {
    vec![
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
        Frame::End(proto::Stats {
            elements: 1,
            warnings: 0,
        }),
    ]
}

fn input() -> std::path::PathBuf {
    std::path::PathBuf::from("addcommit_probe.rvt")
}

#[test]
fn add_commit_moves_frames_not_clones() {
    let mut e = Editor::new_in_memory().unwrap();
    let fr = Arc::new(frames());
    e.apply_import_arc(&input(), fr, false).unwrap();
    match e.undo.last() {
        Some(Cmd::Import(_, kept, _)) => assert_eq!(Arc::strong_count(kept), 1),
        _ => panic!("no import step"),
    }
}

#[test]
fn add_ui_apply_is_one_tx_one_undo() {
    let mut e = Editor::new_in_memory().unwrap();
    let before = e.undo.len();
    e.apply_import_arc(&input(), Arc::new(frames()), false)
        .unwrap();
    assert_eq!(e.undo.len(), before + 1);
    match e.undo.last() {
        Some(Cmd::Import(_, _, tidy)) => assert!(!*tidy),
        _ => panic!("no import step"),
    }
    assert!(!e.walls.is_empty());
    e.undo();
    assert!(e.walls.is_empty());
}

#[test]
fn commands_d_has_import_again() {
    let r = ddc_ui::Registry::builtin();
    for id in ["file.import_again", "file.open_kept"] {
        assert!(r.get(id).is_some(), "{id}");
        let key = format!("\"cmd.{id}\"");
        for text in [
            include_str!("../../../ui/data/i18n/en/cmd_reimport.json"),
            include_str!("../../../ui/data/i18n/ru/cmd_reimport.json"),
        ] {
            assert!(text.contains(&key), "{key}");
        }
    }
}

fn ops(e: &Editor) -> i64 {
    e.model
        .sql()
        .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn add_memory_model_one_undo() {
    let mut e = Editor::new_in_memory().unwrap();
    let old = e.level;
    let (steps, n) = (e.undo.len(), ops(&e));
    e.apply_import_arc(&input(), Arc::new(frames()), true)
        .unwrap();
    assert_eq!(e.undo.len(), steps + 1);
    assert_eq!(ops(&e), n + 1, "import and tidy are one journal record");
    let has = |e: &Editor| e.model.level_list().unwrap().iter().any(|l| l.0 == old);
    assert!(!has(&e), "the untouched default level went with the Add");
    assert_ne!(e.level, old);
    e.undo();
    assert!(has(&e), "one undo brings it back");
    assert!(e.walls.is_empty());
    assert_eq!(ops(&e), n);
}
