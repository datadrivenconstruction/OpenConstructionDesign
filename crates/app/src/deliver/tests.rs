// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The delivery on real models: sheets in one undo step, then PDF and IFC written by the
//! existing jobs; cancel, a failure with its next step, retry.

use super::ui::{start, Review};
use super::*;
use crate::layout_contract::app_in;
use ddc_io::sheets::dset_fixture::build_into;
use ddc_ui::Lang;
use serde_json::json;
use std::time::{Duration, Instant};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc-deliver-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn finish(run: &mut Run, ed: &Editor) {
    let end = Instant::now() + Duration::from_secs(180);
    while !run.step(ed) {
        assert!(Instant::now() < end, "the run did not end: {:?}", run.rows());
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn files(run: &Run, kind: Kind) -> Vec<PathBuf> {
    match &run.rows().iter().find(|r| r.kind == kind).expect("row").state {
        State::Done { files, .. } => files.clone(),
        s => panic!("{kind:?}: {s:?}"),
    }
}

fn starts_with(p: &Path, head: &[u8]) -> bool {
    std::fs::read(p).is_ok_and(|b| b.starts_with(head))
}

#[test]
fn outputs_from_the_tool_arguments() {
    let o = Outputs::from_json(&json!({"outputs": ["pdf", "ifc"]})).unwrap();
    assert_eq!(o, Outputs::defaults());
    assert_eq!(o.names(), ["pdf", "ifc"]);
    let o = Outputs::from_json(&json!({"outputs": ["ifc"], "ifc_schema": "IFC2X3"})).unwrap();
    assert!(!o.prints() && o.ifc);
    assert_eq!(o.schema, IfcSchema::Ifc2x3);
    for other in ["dwg", "dxf", "rvt"] {
        let e = Outputs::from_json(&json!({ "outputs": [other] })).unwrap_err();
        assert!(e.contains("pdf or ifc"), "{e}");
    }
    let e = Outputs::from_json(&json!({"outputs": [], "ifc_schema": "IFC5"})).unwrap_err();
    assert!(e.contains("IFC4X3"), "{e}");
    assert!(Outputs::from_json(&json!({})).is_err(), "outputs are required");
}

#[test]
fn target_defaults_and_replaces() {
    let ed = Editor::new_in_memory().unwrap();
    let t = Target::default_for(&ed);
    assert_eq!(t.stem, "project");
    assert!(t.dir.ends_with("project"), "{}", t.dir.display());
    let dir = temp_dir("names");
    std::fs::create_dir_all(&dir).unwrap();
    let t = Target {
        dir: dir.clone(),
        stem: "House".into(),
    };
    std::fs::write(dir.join("House.pdf"), b"x").unwrap();
    let o = Outputs::defaults();
    assert_eq!(t.replaces(&o), vec![dir.join("House.pdf")]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The whole manual path: one call makes the sheets as one undo step and every file follows.
#[test]
fn drawings_and_every_file_from_one_action() {
    let mut app = app_in(Lang::En);
    let made = build_into(&mut app.ed.model, 2).unwrap();
    app.ed.reload().unwrap();
    let dir = temp_dir("all");
    let mut r = Review::new(&app, false);
    r.outputs = Outputs {
        pdf: true,
        ifc: true,
        schema: IfcSchema::Ifc4x3,
    };
    r.target = Target {
        dir: dir.clone(),
        stem: "House".into(),
    };
    assert_eq!(r.blocked(&app, false), None);
    let undo0 = app.ed.undo_len();
    let mut run = start(&mut app, &r, false).unwrap();
    assert_eq!(app.ed.undo_len(), undo0 + 1, "the sheets are one undo step");
    let sheets = app.ed.model.drawing_set_sheets().unwrap().len();
    assert!(sheets >= 5, "{sheets} sheets");
    finish(&mut run, &app.ed);
    assert_eq!(run.fraction(), 1.0);
    let pdf = files(&run, Kind::Print);
    assert_eq!(pdf[0], dir.join("House.pdf"));
    assert!(starts_with(&pdf[0], b"%PDF"), "a PDF");
    let ifc = files(&run, Kind::Ifc);
    let text = std::fs::read_to_string(&ifc[0]).unwrap();
    assert!(text.contains("IFC4X3_ADD2"), "IFC4X3 header");
    let doors = text
        .lines()
        .filter_map(|l| l.split_once('=').map(|(_, r)| r.trim_start()))
        .filter(|r| r.starts_with("IFCDOOR(") || r.starts_with("IFCDOORSTANDARDCASE("))
        .count();
    assert!(doors >= made.doors, "{doors} doors in the IFC of {}", made.doors);
    assert_eq!(run.file_of(Kind::Ifc), Some(ifc[0].as_path()));
    assert!(run.retry().is_none(), "nothing to run again");
    // Undo takes the sheets back; the files stay as written
    app.ed.undo();
    assert!(app.ed.model.drawing_set_sheets().unwrap().is_empty());
    assert!(pdf[0].is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_before_the_first_step_writes_nothing_and_can_run_again() {
    let ed = Editor::new_in_memory().unwrap();
    let dir = temp_dir("cancel");
    let t = Target {
        dir: dir.clone(),
        stem: "x".into(),
    };
    let o = Outputs::defaults();
    let mut run = Run::start(o, t, vec![], false);
    run.cancel();
    assert!(run.step(&ed), "a cancelled run has ended");
    assert!(run.rows().iter().all(|r| r.state == State::Cancelled));
    assert!(!dir.exists(), "nothing written");
    let again = run.retry().expect("the cancelled outputs again");
    assert_eq!(again.outputs().names(), ["pdf", "ifc"]);
}

/// A failure says what to do next, in the language asked, and the rest still runs.
#[test]
fn failures_name_the_next_step_and_the_rest_runs() {
    for ru in [false, true] {
        let ed = Editor::new_in_memory().unwrap();
        let dir = temp_dir(if ru { "fail-ru" } else { "fail-en" });
        let t = Target {
            dir: dir.clone(),
            stem: "x".into(),
        };
        let o = Outputs::defaults();
        let mut run = Run::start(o, t, vec![], ru);
        finish(&mut run, &ed);
        let State::Failed(why) = &run.rows()[0].state else {
            panic!("{:?}", run.rows());
        };
        assert_eq!(why, &crate::tr_lang(ru, "deliver.err.no_sheets", &[]));
        assert!(why.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)) == ru, "{why}");
        assert!(files(&run, Kind::Ifc)[0].is_file(), "the IFC is written all the same");
        let again = run.retry().unwrap();
        assert_eq!(again.outputs().names(), ["pdf"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_folder_that_cannot_be_made_is_a_failure_with_its_reason() {
    let ed = Editor::new_in_memory().unwrap();
    let file = temp_dir("blocked");
    std::fs::write(&file, b"a file where the folder would go").unwrap();
    let t = Target {
        dir: file.join("out"),
        stem: "x".into(),
    };
    let o = Outputs::from_json(&json!({"outputs": ["ifc"]})).unwrap();
    let mut run = Run::start(o, t, vec![], false);
    finish(&mut run, &ed);
    let State::Failed(why) = &run.rows()[0].state else {
        panic!("{:?}", run.rows());
    };
    assert!(why.starts_with("Can't create the folder"), "{why}");
    let _ = std::fs::remove_file(&file);
}

#[test]
fn review_says_why_it_cannot_run() {
    let app = app_in(Lang::En);
    let mut r = Review::new(&app, false);
    r.drawings = false;
    r.outputs = Outputs::none();
    assert_eq!(r.blocked(&app, false).unwrap(), crate::tr_lang(false, "deliver.why.nothing", &[]));
    r.outputs.pdf = true;
    let want = crate::tr_lang(false, "deliver.why.no_sheets", &[]);
    assert_eq!(r.blocked(&app, false).unwrap(), want);
    r.outputs = Outputs::from_json(&json!({"outputs": ["ifc"]})).unwrap();
    assert_eq!(r.blocked(&app, false), None, "an IFC needs no sheets");
    // an empty model has no walls to draw: the drawing set says so
    r.drawings = true;
    r.dset.refresh(&app.ed, false);
    assert!(r.blocked(&app, false).is_some());
    r.brief = "  a two-storey house  ".into();
    let ask = r.ask_text(false);
    assert!(ask.starts_with("a two-storey house\n"), "{ask}");
    assert!(ask.contains("deliver_project") && ask.contains("ifc, ifc_schema IFC4X3"), "{ask}");
}

#[test]
fn defaults_follow_the_language() {
    let app = app_in(Lang::Ru);
    let ru = Review::new(&app, true);
    let en = Review::new(&app, false);
    assert_eq!(ru.dset.options().standard, "gost");
    assert_eq!(en.dset.options().standard, "iso");
    assert!(ru.drawings && ru.outputs.pdf && ru.outputs.ifc);
    assert_eq!(ru.outputs.schema, IfcSchema::Ifc4x3);
}
