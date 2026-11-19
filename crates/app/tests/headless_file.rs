// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc-app --file`: argument parsing, counts of a real IFC file, exit code on errors,
//! underlay entity count, save + reopen, workbook dry run.

mod e2e_common;

use std::sync::Arc;

use ddc_app::editor::{Editor, OpFn};
use ddc_app::headless_file::{self as hf, FileArgs};
use ddc_model::{Model, OpeningSpec, WallSpec};
use e2e_common::temp_file;

fn argv(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn no_render(_: hf::Loaded, _: &FileArgs) -> Result<serde_json::Value, String> {
    Err("render is not expected here".into())
}

fn report_of(a: &FileArgs) -> (i32, serde_json::Value) {
    let code = hf::run(a, &mut no_render);
    let text = std::fs::read_to_string(a.report.as_ref().expect("report path")).unwrap();
    (code, serde_json::from_str(&text).unwrap())
}

/// Two walls and a door, written as an IFC file.
fn ifc_fixture() -> std::path::PathBuf {
    let mut ed = Editor::new_in_memory().unwrap();
    let (l, t) = (ed.level, ed.types[1].id);
    for (a, b) in [((0.0, 0.0), (6.0, 0.0)), ((6.0, 0.0), (6.0, 4.0))] {
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            m.add_wall(&WallSpec {
                a,
                b,
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .map(drop)
        })))
        .unwrap();
    }
    let wall = e2e_common::ids(&ed.model, "wall")[0];
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.add_opening(
            wall,
            &OpeningSpec {
                along: 1.0,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            },
        )
        .map(drop)
    })))
    .unwrap();
    let text = ddc_io::ifc::export_ifc(&ed.model).unwrap();
    let p = temp_file("headless_fixture.ifc");
    std::fs::write(&p, text).unwrap();
    p
}

#[test]
fn args_file_report_parse() {
    assert!(hf::parse(&argv(&["--screenshot", "a.png"])).unwrap().is_none());
    let a = hf::parse(&argv(&[
        "--file", "m.ifc", "--report-json", "r.json", "--screenshot", "o.png", "--view", "both",
        "--size", "800x600", "--underlay", "p.dxf", "--save", "s.ddc", "--xlsx",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(a.file.to_str(), Some("m.ifc"));
    assert_eq!(a.report.as_deref().and_then(|p| p.to_str()), Some("r.json"));
    assert_eq!(a.screenshot.as_deref().and_then(|p| p.to_str()), Some("o.png"));
    assert_eq!((a.view.as_str(), a.size), ("both", (800, 600)));
    assert!(a.underlay.is_some() && a.save.is_some() && a.xlsx);
    // defaults; '-' means stdout
    let d = hf::parse(&argv(&["--file", "m.ddc", "--report-json", "-"]))
        .unwrap()
        .unwrap();
    assert_eq!((d.view.as_str(), d.size, d.xlsx), ("plan", (1600, 1000), false));
    assert!(d.level.is_none());
    let s = hf::parse(&argv(&["--file", "m.ddc", "--view", "all", "--level", "-1"]))
        .unwrap()
        .unwrap();
    assert_eq!((s.view.as_str(), s.level.as_deref()), ("all", Some("-1")));
    assert!(hf::parse(&argv(&["--file", "m.ddc", "--view", "section"])).is_ok());
    assert!(d.report.is_none() && d.screenshot.is_none());
    // malformed flags are errors, not silent defaults
    assert!(hf::parse(&argv(&["--file"])).is_err());
    assert!(hf::parse(&argv(&["--file", "m.ifc", "--view", "section:1"])).is_err());
    assert!(hf::parse(&argv(&["--file", "m.ifc", "--size", "800"])).is_err());
}

#[test]
fn headless_ifc_fixture_report_counts() {
    let ifc = ifc_fixture();
    let out = temp_file("headless_report.json");
    let a = hf::parse(&argv(&[
        "--file",
        ifc.to_str().unwrap(),
        "--report-json",
        out.to_str().unwrap(),
        "--xlsx",
    ]))
    .unwrap()
    .unwrap();
    let (code, r) = report_of(&a);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["schema"], hf::SCHEMA);
    assert_eq!((r["ok"].as_bool(), r["kind"].as_str()), (Some(true), Some("ifc")));
    assert_eq!(r["counts"]["wall"], 2);
    assert!(r["counts"]["opening"].as_i64().unwrap() >= 1);
    assert!(r["import_ms"].as_f64().unwrap() >= 0.0);
    // every category key is always present
    for k in ["level", "slab", "room", "column", "mesh", "sheet"] {
        assert!(r["counts"][k].is_i64(), "counts.{k}");
    }
    // an unchanged book dry-runs to no changes
    assert!(r["xlsx"]["changed"].is_u64(), "{}", r["xlsx"]);
    assert!(r["underlay"].is_null() && r["save"].is_null() && r["render"].is_null());
}

#[test]
fn headless_missing_file_exit_code() {
    let out = temp_file("headless_missing.json");
    let a = hf::parse(&argv(&[
        "--file",
        "definitely/not/here.ifc",
        "--report-json",
        out.to_str().unwrap(),
    ]))
    .unwrap()
    .unwrap();
    let (code, r) = report_of(&a);
    assert_eq!(code, 1);
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("not found"));
    // wrong type and a broken file are errors too
    let bad = temp_file("headless_bad.ifc");
    std::fs::write(&bad, b"not a step file").unwrap();
    let a = hf::parse(&argv(&[
        "--file",
        bad.to_str().unwrap(),
        "--report-json",
        out.to_str().unwrap(),
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(report_of(&a).0, 1);
    let txt = temp_file("headless_x.txt");
    std::fs::write(&txt, b"x").unwrap();
    let a = hf::parse(&argv(&[
        "--file",
        txt.to_str().unwrap(),
        "--report-json",
        out.to_str().unwrap(),
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(report_of(&a).0, 1);
}

#[test]
fn headless_dxf_underlay_save_reopen() {
    let dxf = temp_file("headless_plan.dxf");
    ddc_io::dxf::write_file(&ddc_io::drawing_model::fixture::sample_plan(0), &dxf).unwrap();
    let ddc = temp_file("headless_out.ddc");
    let out = temp_file("headless_dxf.json");
    let a = hf::parse(&argv(&[
        "--file",
        dxf.to_str().unwrap(),
        "--save",
        ddc.to_str().unwrap(),
        "--report-json",
        out.to_str().unwrap(),
    ]))
    .unwrap()
    .unwrap();
    let (code, r) = report_of(&a);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["kind"], "dxf");
    assert!(r["underlay"]["entities"].as_i64().unwrap() > 0);
    assert_eq!(r["save"]["reopen_match"], true);
    assert!(r["save"]["bytes"].as_u64().unwrap() > 0);
    // the saved project opens as the main file
    let b = hf::parse(&argv(&[
        "--file",
        ddc.to_str().unwrap(),
        "--report-json",
        out.to_str().unwrap(),
    ]))
    .unwrap()
    .unwrap();
    let (code, r2) = report_of(&b);
    assert_eq!(code, 0, "{r2}");
    assert_eq!(r2["kind"], "ddc");
    assert_eq!(r2["counts"], r["counts"]);
}

fn stamp(p: &std::path::Path) -> (Vec<u8>, std::time::SystemTime) {
    (
        std::fs::read(p).unwrap(),
        std::fs::metadata(p).unwrap().modified().unwrap(),
    )
}

#[test]
fn headless_leaves_input_untouched() {
    let ifc = ifc_fixture();
    let ddc = temp_file("headless_untouched.ddc");
    let out = temp_file("headless_untouched.json");
    let run_on = |file: &std::path::Path, extra: &[&str]| {
        let mut v = vec!["--file", file.to_str().unwrap(), "--report-json", out.to_str().unwrap()];
        v.extend_from_slice(extra);
        let a = hf::parse(&argv(&v)).unwrap().unwrap();
        report_of(&a)
    };
    // make a project file from the IFC, then open each input and compare bytes and mtime
    let (code, r) = run_on(&ifc, &["--save", ddc.to_str().unwrap()]);
    assert_eq!(code, 0, "{r}");
    for f in [&ifc, &ddc] {
        let before = stamp(f);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let (code, r) = run_on(f, &["--xlsx"]);
        assert_eq!(code, 0, "{r}");
        assert!(r["size_bytes"].as_u64().unwrap() > 0);
        assert_eq!(stamp(f), before, "input changed: {}", f.display());
    }
    // --save onto the input is refused and leaves it alone
    let before = stamp(&ifc);
    let (code, r) = run_on(&ifc, &["--save", ifc.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(r["error"].as_str().unwrap().contains("input"));
    assert_eq!(stamp(&ifc), before);
    // a report or a frame aimed at the input is refused too; the report then goes to stdout
    for flag in ["--report-json", "--screenshot"] {
        let mut v = vec!["--file", ifc.to_str().unwrap(), flag, ifc.to_str().unwrap()];
        if flag == "--screenshot" {
            v.extend_from_slice(&["--report-json", out.to_str().unwrap()]);
        }
        let a = hf::parse(&argv(&v)).unwrap().unwrap();
        assert_eq!(hf::run(&a, &mut no_render), 1, "{flag}");
        assert_eq!(stamp(&ifc), before, "{flag}");
    }
}

#[test]
fn headless_missing_value_is_clear_error() {
    let e = hf::parse(&argv(&["--file"])).err().unwrap();
    assert!(e.contains("--file") && e.contains("needs a value"), "{e}");
    let e = hf::parse(&argv(&["--file", "--xlsx"])).err().unwrap();
    assert!(e.contains("needs a value"), "{e}");
}

/// A model-check finding (opening above a lowered wall top) is a warning in the report, not an
/// abort: the import keeps its counts.
#[test]
fn headless_integrity_findings_are_warnings() {
    let mut ed = Editor::new_in_memory().unwrap();
    let (l, t) = (ed.level, ed.types[1].id);
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        let w = m.add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })?;
        m.add_opening(
            w,
            &OpeningSpec {
                along: 1.0,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            },
        )?;
        m.set_wall_top_profile(w, Some(&[(0.0, 1.0), (6.0, 1.0)]))
    })))
    .unwrap();
    let src = temp_file("headless_warn.ddc");
    let _ = std::fs::remove_file(&src);
    ed.save_project_as(&src).unwrap();
    let out = temp_file("headless_warn.json");
    let a = hf::parse(&argv(&[
        "--file",
        src.to_str().unwrap(),
        "--report-json",
        out.to_str().unwrap(),
    ]))
    .unwrap()
    .unwrap();
    let (code, r) = report_of(&a);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["ok"], true);
    assert_eq!(r["counts"]["wall"], 1);
    let w = r["warnings"].as_array().unwrap();
    assert!(
        w.iter().any(|x| x["problem"].as_str().unwrap().contains("above wall top")),
        "{r}"
    );
}

/// A panic inside the pass still writes a report with ok=false.
#[test]
fn headless_panic_still_writes_a_report() {
    let src = ifc_fixture();
    let out = temp_file("headless_panic.json");
    let shot = temp_file("headless_panic.png");
    let a = hf::parse(&argv(&[
        "--file",
        src.to_str().unwrap(),
        "--screenshot",
        shot.to_str().unwrap(),
        "--report-json",
        out.to_str().unwrap(),
    ]))
    .unwrap()
    .unwrap();
    let code = hf::run(&a, &mut |_, _| panic!("renderer blew up"));
    let r: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(code, 1);
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("renderer blew up"), "{r}");
}

fn run_args(v: &[&str]) -> (i32, serde_json::Value) {
    let a = hf::parse(&argv(v)).unwrap().unwrap();
    report_of(&a)
}

fn p(path: &std::path::Path) -> &str {
    path.to_str().unwrap()
}

fn file_schema(path: &std::path::Path) -> String {
    let t = std::fs::read_to_string(path).unwrap();
    let at = t.find("FILE_SCHEMA").unwrap();
    t[at..].split('\'').nth(1).unwrap().to_string()
}

/// The two walls of the IFC fixture as a saved project, with one empty sheet.
fn project_with_sheet() -> std::path::PathBuf {
    let ifc = ifc_fixture();
    let ddc = temp_file("headless_sheet.ddc");
    let mk = temp_file("mk.json");
    let (code, r) = run_args(&["--file", p(&ifc), "--save", p(&ddc), "--report-json", p(&mk)]);
    assert_eq!(code, 0, "{r}");
    let mut ed = Editor::new_in_memory().unwrap();
    ed.open_project(&ddc).unwrap();
    ed.model.add_sheet(&ddc_model::SheetDef::new("A-1", "Plan")).unwrap();
    ed.save_project().unwrap();
    drop(ed);
    ddc
}

#[test]
fn headless_exports_every_format() {
    let ifc = ifc_fixture();
    let rep = temp_file("exp.json");
    let names = ["a.ifc", "c.glb", "d.gltf", "e.xlsx", "f.csv"];
    let outs: Vec<std::path::PathBuf> = names.iter().map(|n| temp_file(n)).collect();
    let mut v = vec!["--file", p(&ifc), "--report-json", p(&rep), "--ifc-schema", "IFC2X3"];
    for o in &outs {
        v.extend_from_slice(&["--export", p(o)]);
    }
    let (code, r) = run_args(&v);
    assert_eq!(code, 0, "{r}");
    let ex = r["exports"].as_array().unwrap();
    assert_eq!(ex.len(), names.len(), "{r}");
    for (e, o) in ex.iter().zip(&outs) {
        assert_eq!(e["path"].as_str(), Some(p(o)));
        assert!(e["bytes"].as_u64().unwrap() > 0, "{e}");
        assert!(e["warnings"].is_array(), "{e}");
    }
    assert_eq!(file_schema(&outs[0]), "IFC2X3");
    assert_eq!(ex[0]["schema"], "IFC2X3");
    assert_eq!(&std::fs::read(&outs[1]).unwrap()[..4], b"glTF");
    assert!(ex[1]["nodes"].as_u64().unwrap() > 0);
    let text = std::fs::read_to_string(&outs[2]).unwrap();
    let gltf: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(gltf["buffers"][0]["uri"].as_str().unwrap().starts_with("data:"));
    assert!(!gltf["accessors"].as_array().unwrap().is_empty());
    assert_eq!(&std::fs::read(&outs[3]).unwrap()[..2], b"PK");
    let csv = std::fs::read_to_string(&outs[4]).unwrap();
    assert!(csv.starts_with("category,id,level,global_id,"), "{csv}");
    let walls: Vec<&str> = csv.lines().filter(|l| l.starts_with("wall,")).collect();
    assert_eq!(walls.len(), 2, "{csv}");
    for w in walls {
        let guid = w.split(',').nth(3).unwrap();
        assert_eq!(guid.len(), 22, "the IFC GlobalId is kept: {w}");
    }
}

#[test]
fn headless_model_export_refuses_drawing_and_source_formats() {
    let ifc = ifc_fixture();
    let rep = temp_file("dxfexp.json");
    for ext in ["dxf", "dwg", "rvt"] {
        let out = temp_file(&format!("refused_out.{ext}"));
        let (code, r) = run_args(&["--file", p(&ifc), "--report-json", p(&rep), "--export", p(&out)]);
        assert_ne!(code, 0, "{ext}: {r}");
        assert!(!out.exists(), "{ext}: nothing written");
    }
}

#[test]
fn headless_ifc_import_drops_the_default_level() {
    let ifc = ifc_fixture();
    let rep = temp_file("lvl.json");
    let back = temp_file("lvl_back.ifc");
    let (code, r) = run_args(&["--file", p(&ifc), "--report-json", p(&rep), "--export", p(&back)]);
    assert_eq!(code, 0, "{r}");
    let count = |f: &std::path::Path| {
        std::fs::read_to_string(f).unwrap().matches("IFCBUILDINGSTOREY(").count()
    };
    assert_eq!(count(&back), count(&ifc), "no extra storey on the way back");
}

#[test]
fn headless_ifc_export_keeps_the_input_schema() {
    let ifc = ifc_fixture();
    let rep = temp_file("schema.json");
    let a23 = temp_file("a23.ifc");
    let (code, r) = run_args(&[
        "--file", p(&ifc), "--report-json", p(&rep), "--export", p(&a23), "--ifc-schema", "ifc2x3",
    ]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(file_schema(&a23), "IFC2X3");
    // an IFC2X3 input stays IFC2X3 without a choice, and its walls come back
    let b = temp_file("b.ifc");
    let (code, r) = run_args(&["--file", p(&a23), "--report-json", p(&rep), "--export", p(&b)]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["counts"]["wall"], 2, "{r}");
    assert_eq!(file_schema(&b), "IFC2X3");
    assert_eq!(r["exports"][0]["schema"], "IFC2X3");
    let c = temp_file("c.ifc");
    let (code, _) = run_args(&[
        "--file", p(&a23), "--report-json", p(&rep), "--export", p(&c), "--ifc-schema", "IFC4",
    ]);
    assert_eq!(code, 0);
    assert_eq!(file_schema(&c), "IFC4");
    // the fixture is IFC4X3: kept as well
    let d = temp_file("d.ifc");
    let (code, _) = run_args(&["--file", p(&ifc), "--report-json", p(&rep), "--export", p(&d)]);
    assert_eq!(code, 0);
    assert_eq!(file_schema(&d), "IFC4X3_ADD2");
    assert!(hf::parse(&argv(&["--file", "m.ifc", "--ifc-schema", "IFC5"])).is_err());
    assert!(hf::parse(&argv(&["--file", "m.ifc", "--export", "m.step"])).is_err());
}

#[test]
fn headless_pdf_and_sheet_exports() {
    let ddc = project_with_sheet();
    let rep = temp_file("pdf.json");
    let pdf = temp_file("set.pdf");
    let dir = temp_file("sheets_dir");
    let (code, r) = run_args(&[
        "--file", p(&ddc), "--report-json", p(&rep), "--export", p(&pdf), "--export-sheets", p(&dir),
    ]);
    assert_eq!(code, 0, "{r}");
    assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));
    assert_eq!(r["exports"][0]["sheets"], 1);
    let files: Vec<&str> = r["sheets"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f.as_str())
        .collect();
    assert!(files.iter().any(|f| f.ends_with(".pdf")), "{files:?}");
    assert!(!files.iter().any(|f| f.ends_with(".dxf")), "{files:?}");
    for f in &files {
        assert!(std::fs::metadata(dir.join(f)).unwrap().len() > 0, "{f}");
    }
    // no sheets: a clear failure, and the exports that ran before it are in the report
    let ifc = ifc_fixture();
    let csv = temp_file("before.csv");
    let none = temp_file("none.pdf");
    let (code, r) = run_args(&[
        "--file", p(&ifc), "--report-json", p(&rep), "--export", p(&csv), "--export", p(&none),
    ]);
    assert_eq!(code, 1);
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("no sheets"), "{r}");
    assert_eq!(r["exports"].as_array().unwrap().len(), 1, "{r}");
    assert!(!none.exists());
}

/// Copy of `src` with the first wall's height cell set to `v`; every sheet is kept.
fn edited_book(src: &std::path::Path, dst: &std::path::Path, v: calamine::Data) {
    use calamine::{open_workbook, Data, Reader, Xlsx};
    let mut wb: Xlsx<_> = open_workbook(src).unwrap();
    let mut out = rust_xlsxwriter::Workbook::new();
    for name in wb.sheet_names() {
        let mut g: Vec<Vec<Data>> = wb
            .worksheet_range(&name)
            .unwrap()
            .rows()
            .map(|r| r.to_vec())
            .collect();
        if name == "Walls" {
            let c = g[0].iter().position(|d| d.to_string() == "height [m]").unwrap();
            g[1][c] = v.clone();
        }
        let ws = out.add_worksheet();
        ws.set_name(&name).unwrap();
        for (i, row) in g.iter().enumerate() {
            for (j, d) in row.iter().enumerate() {
                let (i, j) = (i as u32, j as u16);
                match d {
                    Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                    Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                    Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                    Data::Bool(b) => ws.write_boolean(i, j, *b).map(|_| ()),
                    _ => Ok(()),
                }
                .unwrap();
            }
        }
    }
    out.save(dst).unwrap();
}

fn wall_heights(ddc: &std::path::Path) -> Vec<f64> {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.open_project(ddc).unwrap();
    let mut st = ed.model.sql().prepare("SELECT height FROM wall ORDER BY id").unwrap();
    let h: Vec<f64> = st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    h
}

#[test]
fn headless_import_xlsx_applies_an_edit_and_refuses_a_bad_cell() {
    let ddc = project_with_sheet();
    let rep = temp_file("xl.json");
    let book = temp_file("book.xlsx");
    let (code, r) = run_args(&["--file", p(&ddc), "--report-json", p(&rep), "--export", p(&book)]);
    assert_eq!(code, 0, "{r}");
    let edited = temp_file("edited.xlsx");
    edited_book(&book, &edited, calamine::Data::Float(3.5));
    let out = temp_file("applied.ddc");
    let (code, r) = run_args(&[
        "--file", p(&ddc), "--report-json", p(&rep), "--import-xlsx", p(&edited), "--save", p(&out),
    ]);
    assert_eq!(code, 0, "{r}");
    let x = &r["import_xlsx"];
    assert_eq!(x["changed"], 1, "{x}");
    assert_eq!((x["rejected"].as_u64(), x["applied"].as_bool()), (Some(0), Some(true)), "{x}");
    assert_eq!(r["counts"]["wall"], 2);
    let h = wall_heights(&out);
    assert_eq!(h.len(), 2);
    assert!((h[0] - 3.5).abs() < 1e-9, "the edited wall: {h:?}");
    assert!((h[1] - 3.0).abs() < 1e-9, "the other wall is untouched: {h:?}");
    // a value that is no number: refused, nothing applied, nothing saved
    let bad = temp_file("bad.xlsx");
    edited_book(&book, &bad, calamine::Data::String("tall".into()));
    let out2 = temp_file("refused.ddc");
    let (code, r) = run_args(&[
        "--file", p(&ddc), "--report-json", p(&rep), "--import-xlsx", p(&bad), "--save", p(&out2),
    ]);
    assert_eq!(code, 1, "{r}");
    assert_eq!(r["import_xlsx"]["rejected"], 1, "{r}");
    assert_eq!(r["import_xlsx"]["applied"], false);
    assert!(r["error"].as_str().unwrap().contains("rejected"), "{r}");
    assert!(r["save"].is_null() && !out2.exists());
}

#[test]
fn headless_save_leaves_no_lock_files() {
    let ifc = ifc_fixture();
    let ddc = temp_file("nolock.ddc");
    let rep = temp_file("nl.json");
    let (code, r) = run_args(&["--file", p(&ifc), "--save", p(&ddc), "--report-json", p(&rep)]);
    assert_eq!(code, 0, "{r}");
    for side in [".lock", ".lock.pid"] {
        let s = std::path::PathBuf::from(format!("{}{side}", ddc.display()));
        assert!(!s.exists(), "{} is left", s.display());
    }
}

#[test]
fn headless_press_with_file_is_refused_with_a_report() {
    let e = hf::parse(&argv(&["--file", "m.ifc", "--press", "data.export_dxf"]))
        .err()
        .unwrap();
    assert!(e.contains("--press") && e.contains("--export"), "{e}");
    let rep = temp_file("press.json");
    let v = argv(&["--file", "m.ifc", "--press", "x", "--report-json", p(&rep)]);
    hf::report_parse_error(&v, &e);
    let r: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&rep).unwrap()).unwrap();
    assert_eq!((r["ok"].as_bool(), r["schema"].as_str()), (Some(false), Some(hf::SCHEMA)));
    assert!(r["error"].as_str().unwrap().contains("--press"));
    assert!(hf::no_recovery(&argv(&["--no-recovery"])));
}
