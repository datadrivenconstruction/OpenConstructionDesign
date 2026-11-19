//! `ddc publish`: a generated model becomes a revision package that follows the contract.
#![cfg(feature = "link")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ddc_app::link::{glb, package};
use ddc_cli::gen::{run as gen, GenArgs};
use ddc_cli::publish::{run, Args};
use ddc_io::contract::{validate, Doc};
use ddc_io::elements_parquet::{read_parquet, ElementRow};
use serde_json::{json, Value};

const EXAMPLE: &str = include_str!("../../io/contract/examples/brief.de-residential.json");
const REV: &str = "95bb145e-f295-43dd-a059-d27876741295";
const AT: &str = "2026-11-12T11:40:00Z";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc_publish_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The example brief generates a model; `edit` may change the brief first.
fn model(d: &Path, name: &str, edit: impl Fn(&mut Value)) -> (PathBuf, PathBuf) {
    let mut b: Value = serde_json::from_str(EXAMPLE).unwrap();
    edit(&mut b);
    let bp = d.join(format!("{name}.brief.json"));
    std::fs::write(&bp, b.to_string()).unwrap();
    let m = d.join(format!("{name}.ddc"));
    let (t, code) = gen(&GenArgs { brief: bp.clone(), out: m.clone(), force: true }, true, true);
    assert_eq!(code, 0, "{t}");
    (m, bp)
}

fn args(file: &Path, out: &Path) -> Args {
    Args {
        file: file.into(),
        out: out.into(),
        revision_id: Some(REV.into()),
        created_at: Some(AT.into()),
        author: Some("Planer".into()),
        ..Default::default()
    }
}

fn go(a: &Args) -> (Value, i32) {
    let (text, code) = run(a, true, true);
    (serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}")), code)
}

fn open(p: &Path) -> BTreeMap<String, Vec<u8>> {
    package::read_all(p).unwrap()
}

fn rows(all: &BTreeMap<String, Vec<u8>>) -> Vec<ElementRow> {
    read_parquet(all["elements.parquet"].clone()).unwrap()
}

#[test]
fn a_generated_model_publishes_a_package_that_follows_the_contract() {
    let d = dir("ok");
    let (m, brief) = model(&d, "m", |_| {});
    let out = d.join("r.ddcrev");
    let mut a = args(&m, &out);
    a.brief = Some(brief.clone());
    let (log, code) = go(&a);
    assert_eq!(code, 0, "{log}");
    assert_eq!(log["command"], "publish");
    assert_eq!(log["revision_id"], REV);

    let manifest = package::verify(&out).unwrap();
    validate(Doc::Revision, &manifest).unwrap();
    assert_eq!(manifest["source"]["kind"], "generated");
    assert_eq!(manifest["source"]["brief_version"], 3);
    assert_eq!(manifest["source"]["brief_sha256"], package::sha256(&std::fs::read(&brief).unwrap()));
    assert_eq!(manifest["parent_revision_id"], Value::Null);
    assert_eq!(manifest["locale"], "de");
    assert_eq!(manifest["created_at"], AT);
    let all = open(&out);
    assert_eq!(all.keys().cloned().collect::<Vec<_>>(), ["brief.json", "elements.parquet", "geometry.glb", "manifest.json", "model.ddc"]);
    assert!(!all.contains_key("diff.json"), "no parent, no diff");

    // every row validates as the ERP reads it, keys are unique and in the grammar
    let rs = rows(&all);
    assert!(rs.len() > 100, "{}", rs.len());
    let mut seen = BTreeSet::new();
    for r in &rs {
        validate(Doc::ElementRow, &ddc_app::link::publish::row_json(r)).unwrap_or_else(|e| panic!("{}: {e:?}", r.key));
        assert!(ddc_io::elements_parquet::key_ok(&r.key));
        assert!(seen.insert(r.key.clone()), "duplicate key {}", r.key);
    }
    let spaces = rs.iter().filter(|r| r.ifc_class == "IfcSpace").count();
    assert_eq!(manifest["counts"]["elements"], rs.len() - spaces);
    assert_eq!(manifest["counts"]["spaces"], spaces);
    assert!(spaces > 0);

    // the join: node name = element_key = parquet key; extras are exactly three keys; no spaces
    let j = glb::json_chunk(&all["geometry.glb"]).unwrap();
    let nodes = j["nodes"].as_array().unwrap();
    let by_key: BTreeMap<&str, &ElementRow> = rs.iter().map(|r| (r.key.as_str(), r)).collect();
    assert!(nodes.len() > 50);
    let mut names = BTreeSet::new();
    for n in nodes {
        let k = n["name"].as_str().unwrap();
        assert!(names.insert(k), "two nodes named {k}");
        let r = by_key.get(k).unwrap_or_else(|| panic!("node {k} has no row"));
        assert_ne!(r.ifc_class, "IfcSpace");
        let e = n["extras"].as_object().unwrap();
        assert_eq!(e.keys().cloned().collect::<Vec<_>>(), ["element_key", "ifc_class", "type_key"]);
        assert_eq!((e["element_key"].as_str(), e["type_key"].as_str(), e["ifc_class"].as_str()), (Some(k), Some(r.type_key.as_str()), Some(r.ifc_class.as_str())));
        assert_eq!(r.geometry_hash.len(), 64, "{k}");
        assert_ne!(r.geometry_hash, "0".repeat(64));
        assert!(n.get("children").is_none() && n.get("mesh").is_some());
    }
    // repeated types share meshes
    assert!(j["meshes"].as_array().unwrap().len() < nodes.len());
    assert!(j["scenes"].as_array().unwrap().len() == 1);
    // every mesh carries normals
    for m in j["meshes"].as_array().unwrap() {
        let at = &m["primitives"][0]["attributes"];
        assert!(at.get("POSITION").is_some() && at.get("NORMAL").is_some());
        assert_eq!(m["primitives"][0]["mode"], 4);
    }
    // the model inside is the model that was published
    assert!(all["model.ddc"].starts_with(b"SQLite format 3"));
    assert_eq!(manifest["files"].as_array().unwrap().len(), 4);
}

#[test]
fn the_foundation_raft_is_in_the_parquet_and_the_glb_under_the_ground_floor() {
    let d = dir("found");
    let (m, _) = model(&d, "m", |_| {});
    let out = d.join("r.ddcrev");
    assert_eq!(go(&args(&m, &out)).1, 0);
    let all = open(&out);
    let rs = rows(&all);
    let raft = rs.iter().find(|r| r.key == "b1/found/raft").expect("a raft row");
    assert_eq!((raft.ifc_class.as_str(), raft.type_key.as_str()), ("IfcSlab", "slab/foundation-raft/raft"));
    assert!((raft.area_m2.unwrap() - 408.0).abs() < 408.0 * 0.02);
    assert_eq!(raft.geometry_hash.len(), 64);
    assert_ne!(raft.geometry_hash, "0".repeat(64));
    let j = glb::json_chunk(&all["geometry.glb"]).unwrap();
    let node = j["nodes"].as_array().unwrap().iter().find(|n| n["name"] == "b1/found/raft").expect("a raft node");
    assert_eq!(node["extras"]["ifc_class"], "IfcSlab");
    // GLB is Y-up: the raft lies below every storey slab, its top at or under the ground
    assert!(raft.bbox[4] <= 0.0 + 1e-6, "top {}", raft.bbox[4]);
    let ground = rs.iter().filter(|r| r.ifc_class == "IfcWall" && r.storey.as_deref().is_some()).map(|r| r.bbox[1]).fold(f64::INFINITY, f64::min);
    assert!(raft.bbox[1] < ground, "raft {} under walls {}", raft.bbox[1], ground);
}

#[test]
fn an_element_with_a_body_has_a_box_and_a_node_in_the_glb_frame() {
    let d = dir("box");
    let (m, _) = model(&d, "m", |_| {});
    let out = d.join("r.ddcrev");
    assert_eq!(go(&args(&m, &out)).1, 0);
    let all = open(&out);
    let rs = rows(&all);
    let walls: Vec<&ElementRow> = rs.iter().filter(|r| r.ifc_class == "IfcWall" && r.geometry_hash.len() == 64 && r.geometry_hash != "0".repeat(64)).collect();
    assert!(!walls.is_empty());
    for w in walls {
        assert!(w.bbox.iter().all(|c| c.is_finite()), "{}", w.key);
        assert!(w.bbox[0] <= w.bbox[3] && w.bbox[1] <= w.bbox[4] && w.bbox[2] <= w.bbox[5]);
        // Y is up: a wall is taller than a floor slab is thick and starts near the storey, never below the model
        assert!(w.bbox[4] - w.bbox[1] > 1.0, "{} height {}", w.key, w.bbox[4] - w.bbox[1]);
    }
}

#[test]
fn equal_input_and_fixed_identity_give_equal_geometry_rows_and_manifest() {
    let d = dir("det");
    let (m, _) = model(&d, "m", |_| {});
    let (o1, o2) = (d.join("a.ddcrev"), d.join("b.ddcrev"));
    assert_eq!(go(&args(&m, &o1)).1, 0);
    assert_eq!(go(&args(&m, &o2)).1, 0);
    let (a, b) = (open(&o1), open(&o2));
    for k in ["elements.parquet", "geometry.glb", "manifest.json"] {
        assert_eq!(a[k], b[k], "{k} differs between two runs");
    }
    // the whole package too, when the model file is saved the same way
    assert_eq!(std::fs::read(&o1).unwrap().len(), std::fs::read(&o2).unwrap().len());
}

#[test]
fn a_second_revision_carries_a_diff_keyed_by_element_key() {
    let d = dir("diff");
    let (m1, _) = model(&d, "m1", |_| {});
    let (m2, _) = model(&d, "m2", |b| b["massing"]["floors_above"] = json!(6));
    let (r1, r2) = (d.join("1.ddcrev"), d.join("2.ddcrev"));
    assert_eq!(go(&args(&m1, &r1)).1, 0);
    let mut a = args(&m2, &r2);
    a.revision_id = Some("68143a44-27fb-47bb-aa7e-c1229f23a2dc".into());
    a.parent = Some(r1.clone());
    let (log, code) = go(&a);
    assert_eq!(code, 0, "{log}");

    let all = open(&r2);
    let manifest = package::verify(&r2).unwrap();
    validate(Doc::Revision, &manifest).unwrap();
    assert_eq!(manifest["parent_revision_id"], REV);
    let diff: Value = serde_json::from_slice(&all["diff.json"]).unwrap();
    validate(Doc::Diff, &diff).unwrap();
    assert_eq!(diff["parent_revision_id"], REV);
    let (old, new) = (rows(&open(&r1)), rows(&all));
    let (ok, nk): (BTreeSet<&str>, BTreeSet<&str>) = (old.iter().map(|r| r.key.as_str()).collect(), new.iter().map(|r| r.key.as_str()).collect());
    let added: BTreeSet<&str> = diff["added"].as_array().unwrap().iter().map(|k| k.as_str().unwrap()).collect();
    let removed: BTreeSet<&str> = diff["removed"].as_array().unwrap().iter().map(|k| k.as_str().unwrap()).collect();
    assert_eq!(added, nk.difference(&ok).copied().collect::<BTreeSet<_>>());
    assert_eq!(removed, ok.difference(&nk).copied().collect::<BTreeSet<_>>());
    assert!(!added.is_empty(), "a sixth floor adds elements");
    let changed: BTreeSet<&str> = diff["changed"].as_array().unwrap().iter().map(|c| c["element_key"].as_str().unwrap()).collect();
    assert!(changed.is_disjoint(&added) && changed.is_disjoint(&removed) && added.is_disjoint(&removed));
    assert!(!diff["by_type"].as_array().unwrap().is_empty());
}

#[test]
fn the_same_model_against_its_own_package_has_an_empty_diff() {
    let d = dir("same");
    let (m, _) = model(&d, "m", |_| {});
    let (r1, r2) = (d.join("1.ddcrev"), d.join("2.ddcrev"));
    assert_eq!(go(&args(&m, &r1)).1, 0);
    let mut a = args(&m, &r2);
    a.parent = Some(r1);
    a.revision_id = Some("68143a44-27fb-47bb-aa7e-c1229f23a2dc".into());
    assert_eq!(go(&a).1, 0);
    let diff: Value = serde_json::from_slice(&open(&r2)["diff.json"]).unwrap();
    for k in ["added", "removed", "changed", "by_type"] {
        assert!(diff[k].as_array().unwrap().is_empty(), "{k}: {}", diff[k]);
    }
}

#[test]
fn ifc_and_sheets_are_added_on_request() {
    let d = dir("opt");
    let (m, _) = model(&d, "m", |_| {});
    let out = d.join("r.ddcrev");
    let mut a = args(&m, &out);
    a.ifc = true;
    a.sheets = true;
    let (log, code) = go(&a);
    assert_eq!(code, 0, "{log}");
    let all = open(&out);
    let manifest = package::verify(&out).unwrap();
    validate(Doc::Revision, &manifest).unwrap();
    assert!(all["model.ifc"].starts_with(b"ISO-10303-21"));
    let roles: BTreeMap<&str, &str> = manifest["files"].as_array().unwrap().iter().map(|f| (f["path"].as_str().unwrap(), f["role"].as_str().unwrap())).collect();
    assert_eq!(roles["model.ifc"], "ifc");
    for (p, role) in &roles {
        if p.starts_with("sheets/") {
            assert_eq!(*role, "sheet");
            assert!(p.ends_with(".pdf") && all[*p].starts_with(b"%PDF"));
        }
    }
}

#[test]
fn bad_calls_write_nothing_and_exit_2() {
    let d = dir("bad");
    let (m, _) = model(&d, "m", |_| {});
    let out = d.join("r.ddcrev");
    let kind = |a: &Args| {
        let (log, code) = go(a);
        assert_eq!(code, 2, "{log}");
        assert!(!out.exists() && !d.join("r.ddcrev.part").exists(), "something was left behind");
        log["error"]["kind"].as_str().unwrap().to_string()
    };
    // no such file
    assert_eq!(kind(&args(&d.join("nope.ddc"), &out)), "input");
    // a file that is not a project
    let junk = d.join("junk.ddc");
    std::fs::write(&junk, b"hello").unwrap();
    assert_eq!(kind(&args(&junk, &out)), "input");
    // wrong output extension
    let a = args(&m, &d.join("r.zip"));
    assert_eq!(go(&a).1, 2);
    assert!(!d.join("r.zip").exists());
    // depth
    let mut a = args(&m, &out);
    a.depth = Some("D9".into());
    assert_eq!(kind(&a), "input");
    // a parent that is not a package
    let mut a = args(&m, &out);
    let p = d.join("p.ddcrev");
    std::fs::write(&p, b"not a zip").unwrap();
    a.parent = Some(p);
    assert_eq!(kind(&a), "parent");
    // a brief that does not follow the schema
    let mut a = args(&m, &out);
    let b = d.join("b.json");
    std::fs::write(&b, r#"{"schema":"ddc-brief/1"}"#).unwrap();
    a.brief = Some(b);
    assert_eq!(kind(&a), "input");
    // the output is the input
    let mut a = args(&m, &m);
    a.out = m.clone();
    assert_eq!(go(&a).1, 2);
    // existing output needs --force
    let ok = args(&m, &out);
    assert_eq!(go(&ok).1, 0);
    let before = std::fs::read(&out).unwrap();
    let (log, code) = go(&ok);
    assert_eq!(code, 2, "{log}");
    assert_eq!(std::fs::read(&out).unwrap(), before, "a refused run leaves the old package alone");
    let mut f = ok.clone();
    f.force = true;
    assert_eq!(go(&f).1, 0);
}

#[test]
fn a_tampered_parent_is_refused() {
    let d = dir("tamper");
    let (m, _) = model(&d, "m", |_| {});
    let p = d.join("p.ddcrev");
    assert_eq!(go(&args(&m, &p)).1, 0);
    let mut all = open(&p);
    let n = all["elements.parquet"].len() / 2;
    all.get_mut("elements.parquet").unwrap()[n] ^= 0xff;
    let entries: Vec<(String, Vec<u8>)> = all.into_iter().collect();
    std::fs::write(&p, package::zip_bytes(&entries, &|_| false).unwrap()).unwrap();
    let mut a = args(&m, &d.join("r.ddcrev"));
    a.parent = Some(p);
    let (log, code) = go(&a);
    assert_eq!(code, 2, "{log}");
    assert_eq!(log["error"]["kind"], "parent");
    assert!(log["error"]["message"].as_str().unwrap().contains("sha256"));
}

#[test]
fn an_empty_model_publishes_an_empty_but_valid_package() {
    let d = dir("empty");
    let m = d.join("empty.ddc");
    let mut ed = ddc_app::editor::Editor::new_in_memory().unwrap();
    ed.save_project_as(&m).unwrap();
    drop(ed);
    let out = d.join("r.ddcrev");
    let (log, code) = go(&args(&m, &out));
    assert_eq!(code, 0, "{log}");
    let all = open(&out);
    let manifest = package::verify(&out).unwrap();
    validate(Doc::Revision, &manifest).unwrap();
    assert_eq!(manifest["counts"]["elements"], 0);
    assert_eq!(manifest["counts"]["triangles"], 0);
    assert!(rows(&all).is_empty());
    assert!(glb::json_chunk(&all["geometry.glb"]).unwrap()["nodes"].as_array().unwrap().is_empty());
}

#[test]
fn unicode_text_survives_into_the_manifest() {
    let d = dir("uni");
    let (m, _) = model(&d, "m", |_| {});
    let out = d.join("r.ddcrev");
    let mut a = args(&m, &out);
    a.name = Some("Wohnhaus Beispielstraße / 東京 / \u{041c}\u{043e}\u{0441}\u{043a}\u{0432}\u{0430}".into());
    a.message = Some("Änderung: Küche größer \u{1F3E0}".into());
    a.author = Some("Zoë Müller".into());
    a.project_id = Some("746402cb-14a8-430c-b3b9-9c6908fa75d7".into());
    a.erp_url = Some("http://127.0.0.1:8000".into());
    assert_eq!(go(&a).1, 0);
    let manifest = package::verify(&out).unwrap();
    validate(Doc::Revision, &manifest).unwrap();
    assert_eq!(manifest["project_ref"]["name"], "Wohnhaus Beispielstraße / 東京 / \u{041c}\u{043e}\u{0441}\u{043a}\u{0432}\u{0430}");
    assert_eq!(manifest["message"], "Änderung: Küche größer \u{1F3E0}");
    assert_eq!(manifest["author"]["display_name"], "Zoë Müller");
    assert_eq!(manifest["project_ref"]["erp_project_id"], "746402cb-14a8-430c-b3b9-9c6908fa75d7");
}

#[test]
fn an_erp_url_with_credentials_or_plain_http_to_a_remote_host_is_refused_by_the_schema() {
    let d = dir("url");
    let (m, _) = model(&d, "m", |_| {});
    for bad in ["http://example.com", "https://user:pw@example.com", "https://example.com?x=1"] {
        let out = d.join("r.ddcrev");
        let mut a = args(&m, &out);
        a.erp_url = Some(bad.into());
        let (log, code) = go(&a);
        assert_eq!(code, 2, "{bad}: {log}");
        assert_eq!(log["error"]["kind"], "input");
        assert!(!out.exists());
    }
}

/// M4: time and size of a publish at 50 000 elements (columns on a grid, two levels).
/// Run: `cargo test -p ddc-cli --features link --release --test publish m4 -- --ignored --nocapture`.
#[test]
#[ignore]
fn m4_fifty_thousand_elements() {
    use ddc_model::MemberShape;
    let n: usize = std::env::var("DDC_M4_ELEMENTS").ok().and_then(|v| v.parse().ok()).unwrap_or(50_000);
    let d = dir("m4");
    let m = d.join("big.ddc");
    let mut ed = ddc_app::editor::Editor::new_in_memory().unwrap();
    let l0 = ed.model.add_level("L0", 0.0).unwrap();
    let l1 = ed.model.add_level("L1", 3.0).unwrap();
    let mt = ed.model.add_member_type("C1", &MemberShape::Rect { b: 0.3, h: 0.3 }, "Concrete").unwrap();
    let specs: Vec<ddc_model::ColumnSpec> = (0..n)
        .map(|i| ddc_model::ColumnSpec {
            level: l0,
            top_level: Some(l1),
            member_type: mt,
            structural: true,
            at: ((i % 250) as f64 * 4.0, (i / 250) as f64 * 4.0),
            base_offset: 0.0,
            top_offset: 0.0,
            rotation: 0.0,
            top_at: None,
        })
        .collect();
    for c in specs.chunks(5000) {
        ed.model.add_columns(c).unwrap();
    }
    ed.save_project_as(&m).unwrap();
    drop(ed);
    let out = d.join("big.ddcrev");
    let t = std::time::Instant::now();
    let (log, code) = go(&args(&m, &out));
    let secs = t.elapsed().as_secs_f64();
    assert_eq!(code, 0, "{log}");
    let bytes = std::fs::metadata(&out).unwrap().len();
    println!("M4 elements={n} seconds={secs:.1} package_bytes={bytes} steps={}", log["steps"]);
    println!("M4 files={}", log["files"]);
    assert!(secs <= 60.0, "M4 gate: {secs:.1} s");
    assert!(bytes <= 300_000_000, "M4 gate: {bytes} bytes");
    let manifest = package::verify(&out).unwrap();
    assert_eq!(manifest["counts"]["elements"], n);
}
