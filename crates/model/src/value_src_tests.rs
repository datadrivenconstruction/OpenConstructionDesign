// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Value provenance: the schema step, records through save and reopen, verify as one undo
//! step, a later write dropping the record, and `find_missing`.

use super::*;
use crate::checks::{run_all, CheckScope};
use crate::{Edit, LayerSpec, PVal, ParamSpec, ParamType, WallSpec};

/// Fixed clock: the same model saves to the same bytes on every run.
const T0: i64 = 1_700_000_000;
const T1: i64 = 1_700_000_600;

fn spec(name: &str, per_type: bool) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype: ParamType::Text,
        per_type,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    }
}

/// Three walls of one type, parameters "Fire" (instance) and "Code" (type).
fn model_at(path: &str) -> (Model, Vec<i64>, i64) {
    let mut m = Model::open(path).unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type("w", &[LayerSpec { material: "brick".into(), thickness: 0.2 }])
        .unwrap();
    let mut walls = vec![];
    for i in 0..3 {
        let y = f64::from(i) * 3.0;
        walls.push(
            m.add_wall(&WallSpec {
                a: (0.0, y),
                b: (5.0, y),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap(),
        );
    }
    m.add_param(&spec("Fire", false)).unwrap();
    m.add_param(&spec("Code", true)).unwrap();
    (m, walls, t)
}

fn model() -> (Model, Vec<i64>, i64) {
    model_at(":memory:")
}

fn fire(id: i64, v: &str) -> Edit {
    Edit::ParamValue {
        param: "Fire".into(),
        kind: "wall".into(),
        id,
        value: Some(PVal::Text(v.into())),
    }
}

fn ai(at: i64) -> ValueSrc {
    ValueSrc::ai(SrcKind::Ai, at, "sess-1", "sess-1#3", "fill the fire rating of the walls")
}

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ddc_ag2_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn table_present(m: &Model) -> bool {
    has_table(&m.db).unwrap()
}

/// AI fill as the Apply writes it: the values, then the records, in one undo step.
fn ai_fill(m: &mut Model, ids: &[i64], v: &str, at: i64) {
    let edits: Vec<Edit> = ids.iter().map(|id| fire(*id, v)).collect();
    let cells: Vec<(&str, i64, &str)> = ids.iter().map(|id| ("wall", *id, "Fire")).collect();
    m.atomic("wall_ai_plan", |m| {
        m.apply_edits(&edits)?;
        m.record_value_src(&cells, &ai(at)).map(|_| ())
    })
    .unwrap();
}

#[test]
fn a_file_of_the_previous_version_opens_and_gets_the_table() {
    let d = dir("mig");
    let p = d.join("old.ddc");
    let ps = p.to_str().unwrap();
    {
        let (mut m, w, _) = model_at(ps);
        m.apply_edits(&[fire(w[0], "EI30")]).unwrap();
        m.db.execute_batch("DROP TABLE value_src").unwrap();
        m.db.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            [crate::schema::VERSION - 1],
        )
        .unwrap();
    }
    let mut m = Model::open(ps).unwrap();
    assert!(table_present(&m), "the step adds the table to an older file");
    let v: i64 = m
        .db
        .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, crate::schema::VERSION);
    // the old value is there, with no record: entered before provenance existed
    let w0: i64 = m.db.query_row("SELECT min(id) FROM wall", [], |r| r.get(0)).unwrap();
    assert_eq!(m.cell_values(&[("wall", w0, "Fire")]).unwrap(), vec![Some("EI30".into())]);
    assert!(m.value_src("wall", w0, "Fire").unwrap().is_none());
    assert!(m.value_srcs().unwrap().is_empty());
    // a write after the migration works on the new table
    ai_fill(&mut m, &[w0], "EI60", T0);
    assert_eq!(m.value_src("wall", w0, "Fire").unwrap().unwrap().kind, SrcKind::Ai);
    drop(m);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn an_old_file_with_the_previous_spreadsheet_kind_still_loads() {
    let d = dir("oldkind");
    let p = d.join("old.ddc");
    let ps = p.to_str().unwrap();
    let w0 = {
        let (mut m, w, _) = model_at(ps);
        m.apply_edits(&[fire(w[0], "EI30")]).unwrap();
        m.record_value_src(&[("wall", w[0], "Fire")], &ValueSrc::new(SrcKind::Xlsx, T0)).unwrap();
        m.db.execute("UPDATE value_src SET kind='excel'", []).unwrap();
        w[0]
    };
    let m = Model::open(ps).unwrap();
    assert_eq!(m.value_src("wall", w0, "Fire").unwrap().unwrap().kind, SrcKind::Xlsx);
    assert_eq!(SrcKind::Xlsx.as_str(), "xlsx");
    assert_eq!(SrcKind::parse("xlsx"), Some(SrcKind::Xlsx));
    drop(m);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_fresh_model_has_the_table_and_reopening_writes_nothing() {
    let d = dir("fresh");
    let p = d.join("m.ddc");
    let ps = p.to_str().unwrap();
    {
        let m = Model::open(ps).unwrap();
        assert!(table_present(&m));
    }
    let mut c = rusqlite::Connection::open(ps).unwrap();
    crate::schema::upgrade(&mut c, Some(ps)).unwrap();
    assert_eq!(c.total_changes(), 0, "a current file is left as it is");
    drop(c);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn ai_values_start_unverified_and_carry_the_request() {
    let (mut m, w, _) = model();
    ai_fill(&mut m, &[w[0], w[1]], "EI60", T0);
    let s = m.value_src("wall", w[0], "Fire").unwrap().unwrap();
    assert_eq!(s.kind, SrcKind::Ai);
    assert_eq!(s.at, T0);
    assert_eq!(s.session.as_deref(), Some("sess-1"));
    assert_eq!(s.request.as_deref(), Some("sess-1#3"));
    assert_eq!(s.prompt.as_deref(), Some("fill the fire rating of the walls"));
    assert!(!s.verified() && s.needs_review());
    assert_eq!(m.unverified_ai().unwrap().len(), 2);
    assert!(m.value_src("wall", w[2], "Fire").unwrap().is_none());
}

#[test]
fn the_fill_and_its_records_are_one_undo_step() {
    let (mut m, w, _) = model();
    ai_fill(&mut m, &w, "EI60", T0);
    assert_eq!(m.value_srcs().unwrap().len(), 3);
    m.undo().unwrap();
    assert!(m.value_srcs().unwrap().is_empty());
    assert_eq!(m.cell_values(&[("wall", w[0], "Fire")]).unwrap(), vec![None]);
    // the step before the fill is still there
    assert!(m.param_by_name("Code").unwrap().is_some());
}

#[test]
fn verify_is_one_undo_step_and_only_touches_unverified_records() {
    let (mut m, w, _) = model();
    ai_fill(&mut m, &[w[0], w[1]], "EI60", T0);
    let cells = [("wall", w[0], "Fire"), ("wall", w[1], "Fire"), ("wall", w[2], "Fire")];
    assert_eq!(m.verify_values(&cells, T1).unwrap(), 2, "the third has no record");
    let before = m.op_top().unwrap();
    let s = m.value_src("wall", w[1], "Fire").unwrap().unwrap();
    assert_eq!(s.verified_at, Some(T1));
    assert_eq!(s.kind, SrcKind::Ai, "verifying keeps who set it");
    assert!(m.unverified_ai().unwrap().is_empty());
    // nothing left to verify: no journal entry
    assert_eq!(m.verify_values(&cells, T1).unwrap(), 0);
    assert_eq!(m.op_top().unwrap(), before);
    m.undo().unwrap();
    assert_eq!(m.unverified_ai().unwrap().len(), 2, "undo takes the whole verify back");
    assert_eq!(m.value_srcs().unwrap().len(), 2, "and only the verify, not the fill");
    assert_eq!(m.value_src("wall", w[0], "Fire").unwrap().unwrap().at, T0);
}

#[test]
fn a_later_edit_drops_the_record_and_undo_brings_it_back() {
    let (mut m, w, _) = model();
    ai_fill(&mut m, &[w[0]], "EI60", T0);
    // the same value written again (an unchanged round trip) keeps the record
    m.apply_edits(&[fire(w[0], "EI60")]).unwrap();
    assert!(m.value_src("wall", w[0], "Fire").unwrap().is_some());
    m.apply_edits(&[fire(w[0], "EI90")]).unwrap();
    assert!(m.value_src("wall", w[0], "Fire").unwrap().is_none(), "the person's value now");
    m.undo().unwrap();
    let s = m.value_src("wall", w[0], "Fire").unwrap().unwrap();
    assert_eq!((s.kind, s.at), (SrcKind::Ai, T0));
    // clearing the value leaves no badge either
    m.apply_edits(&[Edit::ParamValue {
        param: "Fire".into(),
        kind: "wall".into(),
        id: w[0],
        value: None,
    }])
    .unwrap();
    assert!(m.value_srcs().unwrap().is_empty());
}

#[test]
fn records_survive_save_and_reopen_with_the_same_bytes() {
    let d = dir("save");
    let (a, b) = (d.join("a.ddc"), d.join("b.ddc"));
    let (mut m, w, t) = model();
    ai_fill(&mut m, &[w[0], w[2]], "EI60", T0);
    // a type value set by a generator, then verified
    m.apply_edits(&[Edit::ParamValue {
        param: "Code".into(),
        kind: "wall_type".into(),
        id: t,
        value: Some(PVal::Text("W1".into())),
    }])
    .unwrap();
    m.record_value_src(&[("wall_type", t, "Code")], &ValueSrc::new(SrcKind::Generator, T0))
        .unwrap();
    m.verify_values(&[("wall_type", t, "Code")], T1).unwrap();
    let want = m.value_srcs().unwrap();
    assert_eq!(want.len(), 3);
    m.snapshot_to(&a).unwrap();
    let back = Model::open(a.to_str().unwrap()).unwrap();
    assert_eq!(back.value_srcs().unwrap(), want);
    back.snapshot_to(&b).unwrap();
    // file headers carry a change counter, so the saves are compared by content
    let again = Model::open(b.to_str().unwrap()).unwrap();
    assert_eq!(again.value_srcs().unwrap(), want);
    assert_eq!(again.content_hash().unwrap(), back.content_hash().unwrap(), "saves differ");
    drop(again);
    drop(back);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_check_names_unverified_ai_values_per_parameter() {
    let (mut m, w, _) = model();
    let run = |m: &Model| run_all(m, Some(&["ai_unverified"]), &CheckScope::All, 100).unwrap();
    assert!(run(&m).is_empty());
    ai_fill(&mut m, &w, "EI60", T0);
    let f = run(&m);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].msg_key, "ai.check.ai_unverified");
    assert_eq!(f[0].args[0].1, "Fire");
    assert_eq!(f[0].args[1].1, "3");
    assert_eq!(f[0].elements.len(), 3);
    // a scope of one element sees only that one
    let one = run_all(
        &m,
        Some(&["ai_unverified"]),
        &CheckScope::Elements(vec![("wall".into(), w[1])]),
        100,
    )
    .unwrap();
    assert_eq!(one[0].args[1].1, "1");
    let cells: Vec<(&str, i64, &str)> = w.iter().map(|id| ("wall", *id, "Fire")).collect();
    m.verify_values(&cells, T1).unwrap();
    assert!(run(&m).is_empty(), "verified values are not reported");
}

#[test]
fn xlsx_and_ifc_values_are_not_reported_as_ai() {
    let (mut m, w, _) = model();
    m.apply_edits(&[fire(w[0], "EI30"), fire(w[1], "EI30")]).unwrap();
    m.record_value_src(&[("wall", w[0], "Fire")], &ValueSrc::new(SrcKind::Xlsx, T0)).unwrap();
    m.record_value_src(&[("wall", w[1], "Fire")], &ValueSrc::new(SrcKind::Ifc, T0)).unwrap();
    assert!(m.unverified_ai().unwrap().is_empty());
    assert_eq!(m.value_srcs().unwrap().len(), 2);
    for k in SrcKind::ALL {
        assert_eq!(SrcKind::parse(k.as_str()), Some(k));
    }
    assert_eq!(SrcKind::parse("robot"), None);
}

#[test]
fn find_missing_counts_the_category_and_lists_the_gaps() {
    let (mut m, w, t) = model();
    m.apply_edits(&[fire(w[1], "EI30")]).unwrap();
    let r = m.find_missing("wall", "Fire").unwrap();
    assert_eq!((r.table, r.total), ("wall", 3));
    assert_eq!(r.ids, vec![w[0], w[2]]);
    // a type parameter: an element lacks it while its type has none
    assert_eq!(m.find_missing("wall", "Code").unwrap().ids.len(), 3);
    m.apply_edits(&[Edit::ParamValue {
        param: "Code".into(),
        kind: "wall_type".into(),
        id: t,
        value: Some(PVal::Text("W1".into())),
    }])
    .unwrap();
    assert!(m.find_missing("wall", "Code").unwrap().ids.is_empty());
    let e = m.find_missing("door", "Fire").unwrap_err().to_string();
    assert!(e.contains("not bound") && e.contains("wall"), "{e}");
    assert!(m.find_missing("wall", "Nope").is_err());
}

#[test]
fn prompt_summary_is_one_line_and_bounded() {
    assert_eq!(prompt_summary("  fill\n the   doors "), "fill the doors");
    let long = "x".repeat(PROMPT_MAX + 50);
    let s = prompt_summary(&long);
    assert_eq!(s.chars().count(), PROMPT_MAX);
    assert!(s.ends_with('\u{2026}'));
}
