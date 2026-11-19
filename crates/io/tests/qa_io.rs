// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! QA sweep of the import/export entry points: malformed input never panics and yields an
//! error, round trips keep counts and non-ASCII names, exports are deterministic, an export
//! never overwrites its input, and a 10k-element model imports in bounded time.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Instant;

use ddc_io::{bcf, dxf, gltf, ifc, pdf, topo, xlsx};
use ddc_model::{LayerSpec, Model, OpeningSpec, RoomSpec, WallSpec};

/// Russian "bedroom"
const RU: &str = "\u{421}\u{43f}\u{430}\u{43b}\u{44c}\u{43d}\u{44f}";
/// Japanese bedroom + Chinese "text"
const CJK: &str = "\u{5BDD}\u{5BA4}\u{4E2D}\u{6587}";

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc_qa_io_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level(&format!("{RU} L1"), 0.0).unwrap();
    let wt = m
        .add_wall_type(
            &format!("W {CJK}"),
            &[
                LayerSpec { material: "Brick".into(), thickness: 0.2 },
                LayerSpec { material: "Insulation".into(), thickness: 0.1 },
            ],
        )
        .unwrap();
    let c = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
    let specs: Vec<WallSpec> = (0..4)
        .map(|k| WallSpec {
            a: c[k],
            b: c[(k + 1) % 4],
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: Some(0.0),
        })
        .collect();
    let ids = m.add_walls(&specs).unwrap();
    let op = |along, width, sill, height, kind: &str| OpeningSpec {
        along,
        width,
        sill,
        height,
        kind: kind.into(),
    };
    m.add_opening(ids[0], &op(1.0, 0.9, 0.0, 2.1, "door")).unwrap();
    m.add_opening(ids[2], &op(2.0, 1.5, 0.9, 1.5, "window")).unwrap();
    m.add_room(&RoomSpec {
        level: lv,
        name: format!("{RU} {CJK}"),
        number: "101".into(),
        point: (3.0, 2.0),
        height: 3.0,
    })
    .unwrap();
    m
}

fn n(m: &Model, sql: &str) -> i64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn cell(m: &Model, sql: &str) -> String {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

/// Runs `f` and fails when it panics; the result is returned for further checks.
fn no_panic<T>(what: &str, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => panic!("panic on {what}"),
    }
}

/// Hostile byte inputs for a reader: empty, tiny, junk, text, a prefix of `good` cut at
/// several points, and `good` with a flipped byte.
fn hostile(good: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<(String, Vec<u8>)> = vec![
        ("empty".into(), vec![]),
        ("one byte".into(), vec![0]),
        ("zeros".into(), vec![0; 4096]),
        ("ff".into(), vec![0xff; 4096]),
        ("text".into(), b"not a model at all\n".to_vec()),
        ("utf8 bom".into(), vec![0xEF, 0xBB, 0xBF]),
    ];
    for k in [1usize, 2, 3, 5, 7, 9] {
        let cut = good.len() * k / 10;
        v.push((format!("cut {k}/10"), good[..cut].to_vec()));
    }
    for k in [1usize, 4, 8] {
        let mut b = good.to_vec();
        let at = b.len() * k / 10;
        b[at] ^= 0xA5;
        v.push((format!("flip {k}/10"), b));
    }
    v
}

fn ifc_text() -> String {
    ifc::export_ifc(&model()).unwrap()
}

// ---------------------------------------------------------------- IFC

#[test]
fn ifc_round_trip_counts_units_and_names() {
    let src = model();
    let text = ifc::export_ifc(&src).unwrap();
    ifc::check_step(&text).unwrap();
    let mut back = Model::open(":memory:").unwrap();
    let rep = ifc::import_ifc(&mut back, text.as_bytes()).unwrap();
    assert_eq!(rep.walls, 4);
    assert_eq!(rep.openings, 2);
    assert_eq!(rep.rooms, 1);
    assert_eq!(rep.levels, 1);
    assert!((rep.length_unit - 1.0).abs() < 1e-9, "metres expected, got {}", rep.length_unit);
    assert_eq!(n(&back, "SELECT COUNT(*) FROM wall"), 4);
    assert_eq!(n(&back, "SELECT COUNT(*) FROM room"), 1);
    assert_eq!(cell(&back, "SELECT name FROM level"), format!("{RU} L1"));
    assert_eq!(cell(&back, "SELECT name FROM room"), format!("{RU} {CJK}"));
    // geometry: the walls span a 6 x 4 footprint
    let (minx, maxx, miny, maxy): (f64, f64, f64, f64) = back
        .sql()
        .query_row(
            "SELECT MIN(MIN(ax,bx)),MAX(MAX(ax,bx)),MIN(MIN(ay,by)),MAX(MAX(ay,by)) FROM wall",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert!((maxx - minx - 6.0).abs() < 1e-3 && (maxy - miny - 4.0).abs() < 1e-3);
}

#[test]
fn ifc_export_is_deterministic() {
    let a = ifc_text();
    let b = ifc_text();
    // Documented: the FILE_NAME header line carries a time stamp; the data section is stable.
    let strip = |s: &str| {
        s.lines().filter(|l| !l.starts_with("FILE_NAME")).collect::<Vec<_>>().join("\n")
    };
    assert_eq!(strip(&a), strip(&b));
}

#[test]
fn ifc_malformed_inputs_error_without_panic() {
    let good = ifc_text().into_bytes();
    for (name, bytes) in hostile(&good) {
        let mut m = Model::open(":memory:").unwrap();
        let r = no_panic(&format!("ifc import: {name}"), || ifc::import_ifc(&mut m, &bytes));
        if name == "empty" || name == "one byte" || name == "zeros" || name == "text" {
            assert!(r.is_err(), "ifc import of {name} must fail");
        }
        no_panic(&format!("check_step: {name}"), || {
            let _ = ifc::check_step(&String::from_utf8_lossy(&bytes));
        });
        no_panic(&format!("topo ifc: {name}"), || {
            let _ = topo::from_ifc(&bytes);
        });
    }
}

#[test]
fn ifc_export_file_is_new_and_not_input() {
    let m = model();
    let p = tmp("export_new.ifc");
    let _ = std::fs::remove_file(&p);
    ifc::export_ifc_file(&m, &p, "export_new.ifc", None).unwrap();
    let first = std::fs::read(&p).unwrap();
    ifc::check_step(&String::from_utf8_lossy(&first)).unwrap();
    // importing that file does not touch it
    let mut back = Model::open(":memory:").unwrap();
    ifc::import_ifc_file(&mut back, &p).unwrap();
    assert_eq!(first, std::fs::read(&p).unwrap(), "import must not modify the input");
    std::fs::remove_file(&p).ok();
}

#[test]
fn ifc_missing_file_is_an_error() {
    let mut m = Model::open(":memory:").unwrap();
    let r = ifc::import_ifc_file(&mut m, Path::new("Z:/no/such/dir/x.ifc"));
    assert!(r.is_err());
}

#[test]
fn ifc_10k_elements_import_time() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type("W", &[LayerSpec { material: "Brick".into(), thickness: 0.2 }])
        .unwrap();
    let specs: Vec<WallSpec> = (0..10_000)
        .map(|k| {
            let (x, y) = ((k % 100) as f64 * 2.0, (k / 100) as f64 * 2.0);
            WallSpec {
                a: (x, y),
                b: (x + 1.5, y),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: Some(0.0),
            }
        })
        .collect();
    m.add_walls(&specs).unwrap();
    let t = Instant::now();
    let text = ifc::export_ifc(&m).unwrap();
    let t_export = t.elapsed();
    let mut back = Model::open(":memory:").unwrap();
    let t = Instant::now();
    let rep = ifc::import_ifc(&mut back, text.as_bytes()).unwrap();
    let t_import = t.elapsed();
    eprintln!(
        "QA_IO 10k walls: export {:?}, import {:?}, ifc {} bytes",
        t_export,
        t_import,
        text.len()
    );
    assert_eq!(rep.walls, 10_000);
    assert!(t_import.as_secs() < 120, "10k import took {t_import:?}");
}

// ---------------------------------------------------------------- DXF / DWG

fn drawing() -> dxf::DxfDocument {
    use dxf::{Circle, Common, Entity, Line, Text};
    let mut d = dxf::DxfDocument::default();
    let on = Common::on_layer(&format!("{RU}_{CJK}"));
    d.entities.push(Entity::Line(Line {
        common: on.clone(),
        start: [1.0, 2.0, 0.0],
        end: [3.5, 4.25, 0.0],
    }));
    d.entities.push(Entity::Circle(Circle {
        common: on.clone(),
        center: [5.0, 5.0, 0.0],
        radius: 1.5,
    }));
    d.entities.push(Entity::Text(Text {
        common: on,
        pos: [1.0, 1.0, 0.0],
        height: 0.25,
        text: format!("{RU} {CJK}"),
        rotation: 0.0,
        style: None,
    }));
    d
}

#[test]
fn dxf_round_trip_keeps_entities_and_non_ascii() {
    let doc = drawing();
    let s = dxf::write_string(&doc);
    assert_eq!(s, dxf::write_string(&doc), "dxf writer must be deterministic");
    let plain = dxf::decode_unicode(&s);
    assert!(plain.contains(&format!("{RU} {CJK}")), "text lost in the dxf bytes");
    let read = dxf::read_str(&s).unwrap();
    assert_eq!(read.entities.len(), 3);
    let mut m = Model::open(":memory:").unwrap();
    let rep = dxf::import_into(&mut m, &read).unwrap();
    assert_eq!((rep.lines, rep.circles, rep.texts), (1, 1, 1));
    let back = dxf::export_from(&m).unwrap();
    assert_eq!(back.entities.len(), 3, "entities lost through the model");
    let s2 = dxf::decode_unicode(&dxf::write_string(&back));
    assert!(s2.contains(&format!("{RU} {CJK}")), "text lost through the model");
    assert!(s2.contains(&format!("{RU}_{CJK}")), "layer name lost through the model");
}

#[test]
fn dxf_dwg_malformed_inputs_error_without_panic() {
    let good = dxf::write_string(&drawing()).into_bytes();
    for (name, bytes) in hostile(&good) {
        let r = no_panic(&format!("dxf read_bytes: {name}"), || dxf::read_bytes(&bytes));
        if name == "empty" || name == "zeros" || name == "text" {
            assert!(r.is_err(), "dxf read of {name} must fail");
        }
        let r = no_panic(&format!("dwg read_bytes: {name}"), || ddc_io::dwg::read_bytes(&bytes));
        assert!(r.is_err(), "dwg read of {name} must fail");
        no_panic(&format!("dwg read_bytes_report: {name}"), || {
            let _ = ddc_io::dwg::read_bytes_report(&bytes);
        });
    }
    // a dwg signature followed by junk
    for sig in ["AC1015", "AC1018", "AC1021", "AC1024", "AC1027", "AC1032"] {
        let mut b = sig.as_bytes().to_vec();
        b.extend(std::iter::repeat(0x5a).take(2000));
        let r = no_panic(&format!("dwg sig {sig}"), || ddc_io::dwg::read_bytes(&b));
        assert!(r.is_err(), "dwg {sig} + junk must fail");
        no_panic(&format!("dxf sniff {sig}"), || {
            let _ = dxf::read_bytes(&b);
        });
    }
}

#[test]
fn dwg_missing_file_is_an_error() {
    assert!(ddc_io::dwg::read_file(Path::new("Z:/no/such/file.dwg")).is_err());
    assert!(dxf::read_file(Path::new("Z:/no/such/file.dxf")).is_err());
}

#[test]
fn dxf_write_file_round_trips_and_input_stays() {
    let doc = drawing();
    let p = tmp("rt.dxf");
    dxf::write_file(&doc, &p).unwrap();
    let before = std::fs::read(&p).unwrap();
    let read = dxf::read_file(&p).unwrap();
    assert_eq!(read.entities.len(), 3);
    assert_eq!(before, std::fs::read(&p).unwrap());
    std::fs::remove_file(&p).ok();
}

// ---------------------------------------------------------------- XLSX

#[test]
fn xlsx_round_trip_and_non_ascii() {
    let mut m = model();
    let p = tmp("rt.xlsx");
    let _ = std::fs::remove_file(&p);
    xlsx::export_xlsx(&m, &p).unwrap();
    // second export: same inputs, structurally identical workbook
    let p2 = tmp("rt2.xlsx");
    let _ = std::fs::remove_file(&p2);
    xlsx::export_xlsx(&m, &p2).unwrap();
    assert_eq!(cells(&p), cells(&p2), "xlsx export must be deterministic in content");
    let all = cells(&p).join("\n");
    assert!(all.contains(RU), "cyrillic level name missing from workbook");
    assert!(all.contains(CJK), "cjk name missing from workbook");
    let before = std::fs::read(&p).unwrap();
    xlsx::import_xlsx(&mut m, &p).unwrap();
    assert_eq!(before, std::fs::read(&p).unwrap(), "import must not modify the workbook");
    assert_eq!(n(&m, "SELECT COUNT(*) FROM wall"), 4);
    std::fs::remove_file(&p).ok();
    std::fs::remove_file(&p2).ok();
}

fn cells(p: &Path) -> Vec<String> {
    use calamine::{open_workbook_auto, Reader};
    let mut wb = open_workbook_auto(p).unwrap();
    let mut out = vec![];
    for name in wb.sheet_names().to_vec() {
        out.push(format!("# {name}"));
        let r = wb.worksheet_range(&name).unwrap();
        for row in r.rows() {
            out.push(row.iter().map(|c| format!("{c}")).collect::<Vec<_>>().join("\t"));
        }
    }
    out
}

#[test]
fn xlsx_malformed_inputs_error_without_panic() {
    let mut m = model();
    let p = tmp("good.xlsx");
    xlsx::export_xlsx(&m, &p).unwrap();
    let good = std::fs::read(&p).unwrap();
    std::fs::remove_file(&p).ok();
    for (name, bytes) in hostile(&good) {
        let bad = tmp(&format!("bad_{}.xlsx", name.replace(['/', ' '], "_")));
        std::fs::write(&bad, &bytes).unwrap();
        let r = no_panic(&format!("xlsx import: {name}"), || xlsx::import_xlsx(&mut m, &bad));
        if name.starts_with("cut") || name == "empty" || name == "zeros" || name == "text" {
            assert!(r.is_err(), "xlsx import of {name} must fail");
        }
        std::fs::remove_file(&bad).ok();
    }
    assert_eq!(n(&m, "SELECT COUNT(*) FROM wall"), 4, "failed imports must not change the model");
    assert!(xlsx::import_xlsx(&mut m, Path::new("Z:/no/such.xlsx")).is_err());
}

// ---------------------------------------------------------------- BCF

#[test]
fn bcf_round_trip_both_versions_non_ascii() {
    use ddc_model::Topic;
    for v in [bcf::BcfVersion::V21, bcf::BcfVersion::V30] {
        let m = model();
        let mut t = Topic::new(&format!("{RU} {CJK}"));
        t.description = format!("{CJK} / {RU}");
        t.author = "a@b.c".into();
        let id = m.topic_add(&t).unwrap();
        let bytes = bcf::export_bytes(&m, Some(&[id]), v).unwrap();
        let again = bcf::export_bytes(&m, Some(&[id]), v).unwrap();
        assert_eq!(bytes, again, "bcf export deterministic");
        let m2 = Model::open(":memory:").unwrap();
        let rep = bcf::import_bytes(&m2, &bytes).unwrap();
        assert_eq!(rep.created, 1);
        let title = cell(&m2, "SELECT title FROM issue");
        assert_eq!(title, format!("{RU} {CJK}"));
    }
}

#[test]
fn bcf_malformed_inputs_error_without_panic() {
    let m = model();
    let id = m.topic_add(&ddc_model::Topic::new("t")).unwrap();
    let good = bcf::export_bytes(&m, Some(&[id]), bcf::BcfVersion::V21).unwrap();
    for (name, bytes) in hostile(&good) {
        let m2 = Model::open(":memory:").unwrap();
        let r = no_panic(&format!("bcf import: {name}"), || bcf::import_bytes(&m2, &bytes));
        if name.starts_with("cut") || name == "empty" || name == "zeros" || name == "text" {
            assert!(r.is_err(), "bcf import of {name} must fail");
        }
    }
}

// ---------------------------------------------------------------- GLB

#[test]
fn glb_writes_valid_container_with_non_ascii_names_deterministically() {
    let mut s = gltf::Scene::new(&format!("{RU} {CJK}"));
    let mat = s.material("M", [1.0, 0.0, 0.0, 1.0], serde_json::Value::Null);
    let pos = [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    s.add_part(&format!("{CJK}{RU}"), mat, &pos, &[[0, 1, 2]], serde_json::Value::Null);
    let a = gltf::write_glb(&s);
    let b = gltf::write_glb(&s);
    match (&a, &b) {
        (Ok(a), Ok(b)) => {
            assert_eq!(a, b, "glb deterministic");
            assert_eq!(&a[0..4], b"glTF");
            let len = u32::from_le_bytes(a[8..12].try_into().unwrap()) as usize;
            assert_eq!(len, a.len());
            let json_len = u32::from_le_bytes(a[12..16].try_into().unwrap()) as usize;
            let json = std::str::from_utf8(&a[20..20 + json_len]).unwrap();
            assert!(json.contains(RU) && json.contains(CJK));
        }
        _ => panic!("glb write failed: {a:?}"),
    }
}

#[test]
fn glb_empty_scene_does_not_panic() {
    let s = gltf::Scene::new("");
    no_panic("empty glb", || {
        let _ = gltf::write_glb(&s);
    });
}

// ---------------------------------------------------------------- PDF

#[test]
fn pdf_import_malformed_inputs_error_without_panic() {
    let junk = hostile(b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< >>\n%%EOF\n");
    for (name, bytes) in junk {
        let mut m = Model::open(":memory:").unwrap();
        let opt = pdf::import::ImportOptions::default();
        let r = no_panic(&format!("pdf import: {name}"), || {
            pdf::import::import_file(&mut m, &bytes, 0, &opt)
        });
        assert!(r.is_err(), "pdf import of {name} must fail");
    }
}

// ---------------------------------------------------------------- terrain

#[test]
fn topo_xyz_edge_cases() {
    for (name, s) in [
        ("empty", ""),
        ("blank lines", "\n\n\n"),
        ("words", "a b c\nd e f\n"),
        ("nan", "NaN NaN NaN\ninf 0 0\n"),
        ("two points", "0 0 0\n1 1 1\n"),
        ("cyrillic header", "\u{445} \u{443} \u{437}\n0 0 0\n1 0 0\n0 1 1\n"),
    ] {
        no_panic(&format!("xyz {name}"), || {
            let _ = topo::parse_xyz(s);
        });
    }
    assert!(topo::parse_xyz("").is_err());
    let ok = topo::parse_xyz("0 0 0\n10 0 1\n0 10 2\n10 10 3\n").unwrap();
    assert_eq!(ok.vertex_count(), 4);
}

#[test]
fn topo_unknown_extension_and_missing_file_error() {
    assert!(topo::read_file(Path::new("x.unknown")).is_err());
    assert!(topo::read_file(Path::new("Z:/no/such.xyz")).is_err());
}

#[test]
fn topo_dxf_and_ifc_files_from_malformed_bytes() {
    for ext in ["dxf", "ifc", "xyz", "csv"] {
        let p = tmp(&format!("junk.{ext}"));
        std::fs::write(&p, [0u8, 1, 2, 3, 255, 254]).unwrap();
        no_panic(&format!("topo {ext}"), || {
            let _ = topo::read_file(&p);
        });
        std::fs::remove_file(&p).ok();
    }
}
