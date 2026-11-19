//! Runs the `ddc` binary on a small project built here and on its IFC export.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ddc_model::{LayerSpec, Model, WallSpec};
use serde_json::Value;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc-cli-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Project with one level and three walls, plus its IFC export.
fn fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    m.add_level("L1", 3.0).unwrap();
    let layers = [LayerSpec {
        material: "masonry".into(),
        thickness: 0.2,
    }];
    let wt = m.add_wall_type("W200", &layers).unwrap();
    for (a, b) in [
        ((0.0, 0.0), (5.0, 0.0)),
        ((5.0, 0.0), (5.0, 4.0)),
        ((5.0, 4.0), (0.0, 4.0)),
    ] {
        let w = WallSpec {
            a,
            b,
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        };
        m.add_wall(&w).unwrap();
    }
    let ddc = dir.join("fixture.ddc");
    m.save_project_to(&ddc, &mut |_, _| Ok(())).unwrap();
    let ifc = dir.join("fixture.ifc");
    ddc_io::ifc::write_ifc_file(&m, &ifc).unwrap();
    (ddc, ifc)
}

fn ddc(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ddc"))
        .args(args)
        .arg("--quiet")
        .output()
        .unwrap()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not json ({e}): {}",
            String::from_utf8_lossy(&o.stdout)
        )
    })
}

#[test]
fn info_json_schema() {
    let d = tmp("info");
    let (ddc_file, _) = fixture(&d);
    let o = ddc(&["info", s(&ddc_file), "--json"]);
    assert_eq!(o.status.code(), Some(0));
    let v = json(&o);
    assert_eq!(v["schema"], "ddc-cli-info/1");
    assert_eq!(v["kind"], "ddc");
    let c = v["counts"].as_object().unwrap();
    for k in [
        "level",
        "wall_type",
        "wall",
        "opening",
        "slab",
        "room",
        "mesh",
        "sheet",
    ] {
        assert!(c[k].is_i64(), "counts.{k}");
    }
    assert_eq!(c["wall"], 3);
    assert_eq!(c["level"], 2);
    let lv = v["levels"].as_array().unwrap();
    assert_eq!(lv.len(), 2);
    assert_eq!(lv[1]["name"], "L1");
    assert_eq!(lv[1]["elevation"], 3.0);
    assert!(v["warnings"].is_array());
    assert_eq!(v["warnings_total"], v["warnings"].as_array().unwrap().len());
}

#[test]
fn info_reads_ifc() {
    let d = tmp("info_ifc");
    let (_, ifc) = fixture(&d);
    let v = json(&ddc(&["info", s(&ifc), "--json"]));
    assert_eq!(v["kind"], "ifc");
    assert_eq!(v["counts"]["wall"], 3);
}

#[test]
fn info_text_and_failure() {
    let d = tmp("info_text");
    let (ddc_file, _) = fixture(&d);
    let o = ddc(&["info", s(&ddc_file)]);
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).contains("wall: 3"));
    let o = ddc(&["info", s(&d.join("none.ifc")), "--json"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(json(&o)["error"].as_str().unwrap().contains("not found"));
}

#[test]
fn open_exports_all_formats() {
    let d = tmp("open");
    let (ddc_file, _) = fixture(&d);
    let (a, b, c) = (d.join("o.ifc"), d.join("o.xlsx"), d.join("o.csv"));
    let o = ddc(&[
        "open",
        s(&ddc_file),
        "--export",
        "ifc",
        s(&a),
        "--export",
        "xlsx",
        s(&b),
        "--export",
        "csv",
        s(&c),
        "--json",
    ]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let v = json(&o);
    assert_eq!(v["schema"], "ddc-cli-log/1");
    assert_eq!(v["status"], "ok");
    assert_eq!(v["steps"].as_array().unwrap().len(), 4);
    assert!(std::fs::read_to_string(&a)
        .unwrap()
        .starts_with("ISO-10303-21"));
    assert!(std::fs::metadata(&b).unwrap().len() > 0);
    let csv = std::fs::read_to_string(&c).unwrap();
    assert!(csv.starts_with("category,id,level,global_id,"), "{csv}");
    assert_eq!(csv.lines().filter(|l| l.starts_with("wall,")).count(), 3);
    // no temporary files are left next to the outputs
    assert!(std::fs::read_dir(&d).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".ddc-tmp")));
}

#[test]
fn open_exports_meshes_and_reads_them_back() {
    use ddc_io::meshx::{self, Units};
    let d = tmp("mesh");
    let (ddc_file, _) = fixture(&d);
    let (st, ob) = (d.join("o.stl"), d.join("o.obj"));
    let o = ddc(&["open", s(&ddc_file), "--export", "stl:mm", s(&st), "--export", "obj", s(&ob), "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let (a, _) = ddc_io::stl::read_bytes(&std::fs::read(&st).unwrap(), "o.stl", Units::Millimetre, None).unwrap();
    let (b, w) = ddc_io::obj::read_file(&ob, ddc_io::obj::ObjOptions::default(), None).unwrap();
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(b.parts.len(), 3, "one group per wall");
    assert!(b.parts.iter().all(|p| p.key.starts_with("wall:")));
    assert_eq!(a.triangle_count(), b.triangle_count());
    let ((alo, ahi), (blo, bhi)) = (a.bbox().unwrap(), b.bbox().unwrap());
    for k in 0..3 {
        assert!((alo[k] - blo[k]).abs() < 1e-3 && (ahi[k] - bhi[k]).abs() < 1e-3);
    }
    assert!((ahi[2] - alo[2] - 3.0).abs() < 1e-3, "walls are 3 m high");
    // a mesh file is an input too: the OBJ comes in as three generic elements
    let csv = d.join("back.csv");
    let o = ddc(&["open", s(&ob), "--export", "csv", s(&csv), "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let text = std::fs::read_to_string(&csv).unwrap();
    assert_eq!(text.lines().filter(|l| l.starts_with("mesh,")).count(), 3, "{text}");
    let (set, _) = meshx::read_file(&ob, None).unwrap();
    assert_eq!(set.triangle_count(), b.triangle_count());
}

/// Project with a pipe bend solid (exact faces), a blend and two sheets.
fn exact_fixture(dir: &Path) -> (PathBuf, f64) {
    use ddc_geom::arc::BulgeRing;
    use ddc_geom::gsolid::{Form, Frame, Path3, Section};
    use ddc_geom::math::{v2, v3};
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    let t8 = std::f64::consts::FRAC_PI_8.tan();
    let circle = BulgeRing {
        pts: vec![v2(0.1, 0.0), v2(-0.1, 0.0)],
        bulges: vec![1.0, 1.0],
    };
    let bend = Form::Sweep {
        profile: Section { outer: circle, holes: vec![] },
        path: Path3 {
            pts: vec![v3(0.0, 0.0, 0.5), v3(2.0, 0.0, 0.5), v3(3.0, 1.0, 0.5), v3(3.0, 3.0, 0.5)],
            bulges: vec![0.0, t8, 0.0],
            smooth: None,
        },
    };
    let square = |a: f64| BulgeRing {
        pts: vec![v2(-a, -a), v2(a, -a), v2(a, a), v2(-a, a)],
        bulges: vec![0.0; 4],
    };
    let hopper = Form::Blend {
        bottom: square(1.0),
        top: square(0.5),
        height: 1.0,
    };
    let mut vol = 0.0;
    for (name, form) in [("bend", bend), ("hopper", hopper)] {
        let id = m
            .add_solid(&ddc_model::SolidSpec {
                level: lv,
                name: name.into(),
                void: false,
                material: String::new(),
                frame: Frame::default(),
                form,
                ifc: None,
            })
            .unwrap();
        vol += m.solid_quantities(id).unwrap().unwrap().volume;
    }
    m.add_sheet(&ddc_model::SheetDef::new("A-101", "Plan")).unwrap();
    m.add_sheet(&ddc_model::SheetDef::new("A-102", "Sections")).unwrap();
    let p = dir.join("exact.ddc");
    m.save_project_to(&p, &mut |_, _| Ok(())).unwrap();
    (p, vol)
}

#[test]
fn open_exports_step_3mf_and_svg() {
    let d = tmp("exact");
    let (src, vol) = exact_fixture(&d);
    let (st, tm, sv) = (d.join("o.step"), d.join("o.3mf"), d.join("o.svg"));
    let o = ddc(&[
        "open", s(&src), "--export", "step", s(&st), "--export", "3mf:cm", s(&tm), "--export", "svg", s(&sv), "--json",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(json(&o)["status"], "ok");
    // STEP: exact B-spline faces of the bend, planes of the hopper, same volume
    let text = std::fs::read_to_string(&st).unwrap();
    assert!(text.contains("FILE_SCHEMA(('AUTOMOTIVE_DESIGN"));
    assert!(text.contains("B_SPLINE_SURFACE"), "the bend has exact faces");
    assert!(text.contains("SI_UNIT(.MILLI.,.METRE.)"));
    let (doc, warns) = ddc_io::step::read_exact(text.as_bytes()).unwrap();
    assert!(warns.is_empty(), "{warns:?}");
    assert_eq!(doc.parts.len(), 2);
    let got: f64 = doc.parts.iter().map(|p| ddc_io::exact::volume(&p.body).unwrap()).sum();
    assert!((got - vol).abs() <= 1e-3 * vol, "{got} vs {vol}");
    // 3MF: a zip with the core model part in centimetres, two objects
    let mut z = zip::ZipArchive::new(std::fs::File::open(&tm).unwrap()).unwrap();
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut z.by_name("3D/3dmodel.model").unwrap(), &mut xml).unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let root = doc.root_element();
    assert_eq!(root.tag_name().name(), "model");
    assert_eq!(root.attribute("unit"), Some("centimeter"));
    assert_eq!(doc.descendants().filter(|n| n.has_tag_name("object")).count(), 2);
    // SVG: one file per sheet, each parses and has lines
    let other = d.join("o-A-102.svg");
    for p in [&sv, &other] {
        let t = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let x = roxmltree::Document::parse(&t).unwrap();
        assert_eq!(x.root_element().tag_name().name(), "svg");
        assert!(x.descendants().any(|n| ["polyline", "polygon"].iter().any(|t| n.has_tag_name(*t))), "no line in {}", p.display());
    }
    // a unit the format does not know is refused before anything is written
    let bad = ddc(&["open", s(&src), "--export", "3mf:yd", s(&d.join("x.3mf"))]);
    assert_ne!(bad.status.code(), Some(0));
}

#[test]
fn open_exports_a_freeform_shell_to_step_exactly() {
    use ddc_geom::math::v3;
    use ddc_geom::nurbs::{Surface, Trim};
    let d = tmp("freeform-step");
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    let ctrl = (0..9).map(|k| v3(2.0 * (k / 3) as f64, 2.0 * (k % 3) as f64, if k == 4 { 1.0 } else { 0.0 })).collect();
    let knots = vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    m.add_freeform(&ddc_model::FreeformSpec {
        level: lv,
        name: "canopy".into(),
        material: String::new(),
        surface: Surface::new(2, 2, knots.clone(), knots, 3, 3, ctrl, vec![1.0; 9]).unwrap(),
        thickness: 0.2,
        side: ddc_model::ShellSide::Normal,
        trim: Trim::default(),
        origin: "points".into(),
    })
    .unwrap();
    let src = d.join("ff.ddc");
    m.save_project_to(&src, &mut |_, _| Ok(())).unwrap();
    let st = d.join("ff.step");
    let o = ddc(&["open", s(&src), "--export", "step", s(&st), "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let text = std::fs::read_to_string(&st).unwrap();
    assert!(text.contains("B_SPLINE_SURFACE_WITH_KNOTS"), "the patch is written as NURBS");
    assert!(text.contains("MANIFOLD_SOLID_BREP") && text.contains("CLOSED_SHELL"), "the shell is a solid");
    let (doc, _) = ddc_io::step::read_exact(text.as_bytes()).unwrap();
    assert_eq!(doc.parts.len(), 1);
    assert_eq!(doc.parts[0].body.faces.len(), 6, "two offset faces and four ruled sides");
}

#[test]
fn open_ifc_to_csv() {
    let d = tmp("open_ifc");
    let (_, ifc) = fixture(&d);
    let out = d.join("e.csv");
    let o = ddc(&["open", s(&ifc), "--export", "csv", s(&out)]);
    assert_eq!(o.status.code(), Some(0));
    assert!(std::fs::read_to_string(&out).unwrap().contains("wall,"));
}

#[test]
fn ifc_export_keeps_input_schema_or_the_chosen_one() {
    let d = tmp("ifc_schema");
    let (_, ifc4x3) = fixture(&d);
    let schema = |p: &Path| {
        let t = std::fs::read_to_string(p).unwrap();
        let at = t.find("FILE_SCHEMA").unwrap();
        t[at..].split('\'').nth(1).unwrap().to_string()
    };
    let out = |n: &str| d.join(n);
    // an IFC2X3 input stays IFC2X3 unless a schema is named
    let o = ddc(&["open", s(&ifc4x3), "--export", "ifc2x3", s(&out("a23.ifc"))]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stdout));
    assert_eq!(schema(&out("a23.ifc")), "IFC2X3");
    let o = ddc(&["open", s(&out("a23.ifc")), "--export", "ifc", s(&out("b.ifc"))]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stdout));
    assert_eq!(schema(&out("b.ifc")), "IFC2X3");
    let o = ddc(&["open", s(&out("a23.ifc")), "--export", "ifc4", s(&out("c.ifc"))]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(schema(&out("c.ifc")), "IFC4");
    let o = ddc(&["open", s(&out("c.ifc")), "--export", "ifc", s(&out("d.ifc"))]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(schema(&out("d.ifc")), "IFC4");
    let o = ddc(&["open", s(&out("c.ifc")), "--export", "ifc4x3", s(&out("e.ifc"))]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(schema(&out("e.ifc")), "IFC4X3_ADD2");
    // a project has no source schema: the newest one
    let o = ddc(&["open", s(&d.join("fixture.ddc")), "--export", "ifc", s(&out("f.ifc"))]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(schema(&out("f.ifc")), "IFC4X3_ADD2");
}

#[test]
fn refuses_to_overwrite_input() {
    let d = tmp("refuse");
    let (ddc_file, ifc) = fixture(&d);
    let before = std::fs::read(&ifc).unwrap();
    let o = ddc(&["open", s(&ifc), "--export", "ifc", s(&ifc)]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stdout).contains("refusing to overwrite"));
    assert_eq!(std::fs::read(&ifc).unwrap(), before);
    // the same file spelled differently
    let sneaky = d.join("sub").join("..").join("fixture.ifc");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    let o = ddc(&["open", s(&ifc), "--export", "ifc", s(&sneaky)]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(std::fs::read(&ifc).unwrap(), before);
    // one export refused, one written: partial
    let good = d.join("good.csv");
    let o = ddc(&[
        "open",
        s(&ddc_file),
        "--export",
        "csv",
        s(&ddc_file),
        "--export",
        "csv",
        s(&good),
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert!(good.is_file());
    assert!(Command::new(env!("CARGO_BIN_EXE_ddc"))
        .args(["info", s(&ddc_file), "--quiet"])
        .status()
        .unwrap()
        .success());
}

#[test]
fn usage_errors_exit_2() {
    for a in [
        vec!["open", "x.ifc"],
        vec!["open", "x.ifc", "--export", "dwg", "o"],
        vec!["bogus"],
        vec!["info"],
        vec!["info", "a", "b"],
        vec!["info", "a", "--export", "csv", "o"],
    ] {
        assert_eq!(ddc(&a).status.code(), Some(2), "{a:?}");
    }
    assert_eq!(ddc(&["--help"]).status.code(), Some(0));
    let o = ddc(&["open", "x.txt", "--export", "csv", "o.csv"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn batch_continues_after_failures() {
    let d = tmp("batch");
    let (ddc_file, ifc) = fixture(&d);
    let jobs = serde_json::json!({"jobs": [
        {"id": "a", "file": "fixture.ddc", "info": true,
         "export": [{"format": "csv", "out": "a.csv"}]},
        {"id": "missing", "file": "nope.ifc", "export": [{"format": "csv", "out": "m.csv"}]},
        {"id": "clobber", "file": "fixture.ifc",
         "export": [{"format": "ifc", "out": "fixture.ifc"}]},
        {"id": "b", "file": "fixture.ifc", "export": [{"format": "xlsx", "out": "b.xlsx"}]},
    ]});
    let jf = d.join("jobs.json");
    std::fs::write(&jf, jobs.to_string()).unwrap();
    let before = std::fs::read(&ifc).unwrap();
    let o = ddc(&["batch", s(&jf), "--json"]);
    assert_eq!(o.status.code(), Some(1));
    let v = json(&o);
    assert_eq!(v["exit"], 1);
    assert_eq!(v["summary"]["ok"], 2);
    assert_eq!(v["summary"]["failed"], 2);
    let st: Vec<&str> = v["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["status"].as_str().unwrap())
        .collect();
    assert_eq!(st, ["ok", "failed", "failed", "ok"]);
    assert_eq!(v["jobs"][0]["info"]["counts"]["wall"], 3);
    assert!(d.join("a.csv").is_file() && d.join("b.xlsx").is_file());
    assert!(!d.join("m.csv").exists());
    assert_eq!(std::fs::read(&ifc).unwrap(), before);
    assert!(ddc_file.is_file());
}

#[test]
fn batch_all_failed_and_bad_file() {
    let d = tmp("batch_bad");
    let jf = d.join("jobs.json");
    std::fs::write(&jf, r#"[{"file": "nope.ifc"}]"#).unwrap();
    assert_eq!(ddc(&["batch", s(&jf)]).status.code(), Some(2));
    std::fs::write(&jf, "not json").unwrap();
    assert_eq!(ddc(&["batch", s(&jf)]).status.code(), Some(2));
    std::fs::write(&jf, "[]").unwrap();
    assert_eq!(ddc(&["batch", s(&jf)]).status.code(), Some(2));
    assert_eq!(
        ddc(&["batch", s(&d.join("absent.json"))]).status.code(),
        Some(2)
    );
}

const GOOD: &str = "local p = profile.rect(0.2, 0.2)
part('a', extrude(p, vec3(0, 0, 0), vec3(2, 0, 0)))
part('b', extrude(p, vec3(0, 1, 0), vec3(2, 1, 0)))
";

fn script(dir: &Path, text: &str) -> PathBuf {
    let p = dir.join("s.lua");
    std::fs::write(&p, text).unwrap();
    p
}

#[test]
fn run_saves_a_new_project_and_leaves_the_input() {
    let d = tmp("run_ok");
    let (proj, _) = fixture(&d);
    let before = std::fs::read(&proj).unwrap();
    let sc = script(&d, GOOD);
    let out = d.join("new.ddc");
    let o = ddc(&["run", s(&proj), s(&sc), "--out", s(&out), "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stdout));
    let v = json(&o);
    assert_eq!(v["created"], 2);
    assert!(v["hash"].is_string());
    assert_eq!(std::fs::read(&proj).unwrap(), before);
    let o = ddc(&["info", s(&out), "--json"]);
    assert_eq!(o.status.code(), Some(0));
    let total = |v: &Value| -> i64 {
        v["counts"].as_object().unwrap().values().filter_map(Value::as_i64).sum()
    };
    let old = json(&ddc(&["info", s(&proj), "--json"]));
    assert!(total(&json(&o)) > total(&old));
}

#[test]
fn run_script_error_has_line_and_column() {
    let d = tmp("run_err");
    let (proj, _) = fixture(&d);
    let sc = script(&d, "local p = profile.rect(0.2, 0.2)
local f = extrud(p, vec3(), vec3(1, 0, 0))
");
    let out = d.join("new.ddc");
    let o = ddc(&["run", s(&proj), s(&sc), "--out", s(&out), "--json"]);
    assert_eq!(o.status.code(), Some(2));
    let v = json(&o);
    assert_eq!(v["script_error"]["line"], 2);
    assert!(v["script_error"]["col"].as_u64().unwrap() >= 1);
    assert!(!out.exists(), "nothing is saved on a script error");
}

#[test]
fn run_stops_a_runaway_script() {
    let d = tmp("run_limit");
    let (proj, _) = fixture(&d);
    let sc = script(&d, "local x = 0
while true do
  x = x + 1
end
");
    let out = d.join("new.ddc");
    let o = ddc(&["run", s(&proj), s(&sc), "--out", s(&out), "--json"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(json(&o)["script_error"]["kind"], "limit");
    assert!(!out.exists());
}

#[test]
fn run_refuses_to_overwrite_the_input() {
    let d = tmp("run_same");
    let (proj, _) = fixture(&d);
    let sc = script(&d, GOOD);
    let o = ddc(&["run", s(&proj), s(&sc), "--out", s(&proj), "--json"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn run_needs_out() {
    let o = ddc(&["run", "a.ddc", "s.lua"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn run_on_a_broken_ifc_fails_cleanly_with_json() {
    let d = tmp("run_broken");
    let bad = d.join("bad.ifc");
    std::fs::write(&bad, b"ISO-10303-21;\nDATA;\n#1=IFCWALL(((\n").unwrap();
    let sc = script(&d, GOOD);
    let out = d.join("new.ddc");
    let o = ddc(&["run", s(&bad), s(&sc), "--out", s(&out), "--json"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(json(&o)["exit"], 2);
}

#[test]
fn batch_refuses_to_overwrite_the_jobs_file() {
    let d = tmp("batch_jobs");
    let (proj, _) = fixture(&d);
    let jobs = d.join("jobs.json");
    let text = format!(
        r#"{{"jobs":[{{"file":{:?},"export":[{{"format":"csv","out":"jobs.json"}}]}}]}}"#,
        s(&proj)
    );
    std::fs::write(&jobs, &text).unwrap();
    let o = ddc(&["batch", s(&jobs), "--json"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(&jobs).unwrap(), text);
}

#[test]
fn batch_refuses_two_exports_to_one_file() {
    let d = tmp("batch_dup");
    let (proj, _) = fixture(&d);
    let jobs = d.join("jobs.json");
    let text = format!(
        r#"{{"jobs":[{{"file":{p:?},"export":[{{"format":"csv","out":"a.csv"}}]}},{{"file":{p:?},"export":[{{"format":"csv","out":"a.csv"}}]}}]}}"#,
        p = s(&proj)
    );
    std::fs::write(&jobs, text).unwrap();
    let o = ddc(&["batch", s(&jobs), "--json"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(json(&o)["error"].as_str().unwrap().contains("same file"));
    assert!(!d.join("a.csv").exists());
}

#[cfg(unix)]
#[test]
fn a_non_utf8_file_name_does_not_panic() {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::OsStr::from_bytes(b"/tmp/\xff\xfe.ddc");
    let o = Command::new(env!("CARGO_BIN_EXE_ddc"))
        .arg("info")
        .arg(name)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[cfg(windows)]
#[test]
fn a_non_utf16_file_name_does_not_panic() {
    use std::os::windows::ffi::OsStringExt;
    let name = std::ffi::OsString::from_wide(&[0x61, 0xD800, 0x2E, 0x69, 0x66, 0x63]);
    let o = Command::new(env!("CARGO_BIN_EXE_ddc"))
        .arg("info")
        .arg(name)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn open_refuses_two_exports_to_one_file_even_with_other_case() {
    let d = tmp("open_dup");
    let (proj, _) = fixture(&d);
    let a = d.join("o.csv");
    let b = d.join("O.CSV");
    let o = ddc(&[
        "open", s(&proj), "--export", "csv", s(&a), "--export", "xlsx", s(&b), "--json",
    ]);
    if cfg!(windows) {
        assert_eq!(o.status.code(), Some(2));
        assert!(json(&o)["error"].as_str().unwrap().contains("same file"));
        assert!(!a.exists());
    }
    let o = ddc(&[
        "open", s(&proj), "--export", "csv", s(&a), "--export", "xlsx", s(&a), "--json",
    ]);
    assert_eq!(o.status.code(), Some(2));
    assert!(!a.exists());
}
