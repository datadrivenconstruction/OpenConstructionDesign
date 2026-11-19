use std::path::PathBuf;

use super::p21::{decode, parse};
use super::{read_bytes, read_file};
use crate::job::{Progress, CANCELLED};
use crate::meshx::{MeshSet, Part};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/step").join(name)
}

fn load(name: &str) -> (MeshSet, Vec<String>) {
    read_file(&data(name), None).unwrap_or_else(|e| panic!("{e}"))
}

fn volume(p: &Part) -> f64 {
    p.triangles
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| p.positions[i as usize]);
            (a[0] * (b[1] * c[2] - b[2] * c[1]) + a[1] * (b[2] * c[0] - b[0] * c[2]) + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

fn near(a: [f64; 3], b: [f64; 3], tol: f64) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= tol)
}

fn material<'a>(m: &'a MeshSet, p: &Part) -> &'a str {
    &m.materials[p.material.expect("material")].name
}

#[test]
fn box_planes_metres_and_colour() {
    let (m, warn) = load("box_mm.stp");
    assert!(warn.is_empty(), "{warn:?}");
    assert_eq!(m.parts.len(), 1);
    let p = &m.parts[0];
    assert_eq!(p.triangles.len(), 12);
    assert_eq!(p.name, "Grösse Box");
    let (lo, hi) = p.bbox().unwrap();
    assert!(near(lo, [0.0; 3], 1e-12) && near(hi, [0.1, 0.2, 0.3], 1e-12), "{lo:?} {hi:?}");
    assert!((volume(p) - 0.006).abs() < 1e-9, "{}", volume(p));
    assert_eq!(material(&m, p), "step #cc3333");
}

fn cylinder(name: &str) {
    let (m, warn) = load(name);
    assert!(warn.is_empty(), "{warn:?}");
    assert_eq!(m.parts.len(), 1);
    let p = &m.parts[0];
    let v = volume(p);
    let exact = std::f64::consts::PI * 0.01 * 0.01 * 0.02;
    assert!((v - exact).abs() < exact * 0.01, "{name}: {v} vs {exact}");
    let (lo, hi) = p.bbox().unwrap();
    assert!(near(lo, [-0.01, -0.01, 0.0], 1e-4) && near(hi, [0.01, 0.01, 0.02], 1e-4), "{lo:?} {hi:?}");
}

#[test]
fn cylinder_with_seam() {
    cylinder("cylinder_mm.stp");
}

#[test]
fn cylinder_band_without_seam() {
    cylinder("cylinder_band_mm.stp");
}

#[test]
fn sphere_single_face_with_seam() {
    let (m, warn) = load("sphere_mm.stp");
    assert!(warn.is_empty(), "{warn:?}");
    let p = &m.parts[0];
    let exact = 4.0 / 3.0 * std::f64::consts::PI * 0.01f64.powi(3);
    let v = volume(p);
    assert!((v - exact).abs() < exact * 0.02, "{v} vs {exact}");
    let (lo, hi) = p.bbox().unwrap();
    assert!(near(lo, [-0.01; 3], 2e-4) && near(hi, [0.01; 3], 2e-4), "{lo:?} {hi:?}");
}

#[test]
fn faceted_tetra_in_inches() {
    let (m, warn) = load("tetra_inch.stp");
    assert!(warn.is_empty(), "{warn:?}");
    let p = &m.parts[0];
    assert_eq!(p.triangles.len(), 4);
    let (lo, hi) = p.bbox().unwrap();
    assert!(near(lo, [0.0; 3], 1e-12) && near(hi, [0.0254; 3], 1e-12), "{lo:?} {hi:?}");
    let exact = 0.0254f64.powi(3) / 6.0;
    assert!((volume(p) - exact).abs() < exact * 1e-9, "{}", volume(p));
}

#[test]
fn assembly_instances_placed() {
    let (m, warn) = load("assembly_mm.stp");
    assert!(warn.is_empty(), "{warn:?}");
    assert_eq!(m.parts.len(), 2);
    assert_ne!(m.parts[0].key, m.parts[1].key);
    let mut boxes: Vec<_> = m.parts.iter().map(|p| p.bbox().unwrap()).collect();
    boxes.sort_by(|a, b| a.0[0].total_cmp(&b.0[0]));
    assert!(near(boxes[0].0, [0.0; 3], 1e-12) && near(boxes[0].1, [0.01; 3], 1e-12), "{boxes:?}");
    assert!(near(boxes[1].0, [0.09, 0.0, 0.0], 1e-12) && near(boxes[1].1, [0.1, 0.01, 0.01], 1e-12), "{boxes:?}");
    for p in &m.parts {
        assert!(p.name.contains("Block"), "{}", p.name);
        assert!((volume(p) - 1e-6).abs() < 1e-12);
        assert_eq!(material(&m, p), "step #0000ff");
    }
}

#[test]
fn malformed_entity_is_located() {
    let text = b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=CARTESIAN_POINT('',(0.,0.,0.));\n#2=CARTESIAN_POINT('',(0.,0.,0.) 1.);\nENDSEC;\nEND-ISO-10303-21;\n";
    let e = read_bytes(text, "bad.stp", None).unwrap_err();
    assert!(e.starts_with("bad.stp: ") && e.contains("#2") && e.contains("byte offset"), "{e}");
    assert!(e.contains("line 6"), "{e}");
}

#[test]
fn truncated_file_is_reported() {
    let full = std::fs::read(data("box_mm.stp")).unwrap();
    let e = read_bytes(&full[..full.len() * 2 / 3], "cut.stp", None).unwrap_err();
    assert!(e.contains("truncated"), "{e}");
    let e = read_bytes(b"solid x\nendsolid x\n", "x.stp", None).unwrap_err();
    assert!(e.contains("ISO-10303-21"), "{e}");
}

#[test]
fn file_without_geometry_is_an_error() {
    let text = b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=CARTESIAN_POINT('',(0.,0.,0.));\nENDSEC;\nEND-ISO-10303-21;\n";
    let e = read_bytes(text, "empty.stp", None).unwrap_err();
    assert!(e.contains("no solid"), "{e}");
}

#[test]
fn progress_trail_and_cancel() {
    let p = Progress::new();
    load_with(&p);
    let trail = p.trail();
    assert!(trail.contains(&"job.stage.parse") && trail.contains(&"job.stage.plan"), "{trail:?}");
    let p = Progress::new();
    p.cancel();
    assert_eq!(read_file(&data("box_mm.stp"), Some(&p)).unwrap_err(), CANCELLED);
}

fn load_with(p: &Progress) {
    read_file(&data("cylinder_mm.stp"), Some(p)).unwrap();
}

#[test]
fn string_decoding() {
    assert_eq!(decode(b"it''s"), "it's");
    assert_eq!(decode(b"a\\\\b"), "a\\b");
    assert_eq!(decode(b"\\X\\E9t\\X\\E9"), "été");
    assert_eq!(decode(b"\\X2\\00F600DF\\X0\\"), "öß");
    assert_eq!(decode(b"\\X4\\0001F600\\X0\\"), "\u{1F600}");
    assert_eq!(decode(b"\\S\\i"), "é");
}

#[test]
fn parser_records_and_complex_entities() {
    let text = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('CONFIG_CONTROL_DESIGN'));\nENDSEC;\nDATA;\n\
/* note */ #10 = CARTESIAN_POINT ( 'p' , ( 1. , -2.5E1 , 3 ) ) ;\n\
#11=(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.));\n\
#12=MEASURE(LENGTH_MEASURE(2.),$,#10);\nENDSEC;\nEND-ISO-10303-21;\n";
    let st = parse(text, None).unwrap();
    assert_eq!(st.len(), 3);
    let a = st.rec(10, "CARTESIAN_POINT").unwrap();
    assert_eq!(a[0].text(), Some("p"));
    assert_eq!(a[1].nums(), vec![1.0, -25.0, 3.0]);
    assert!(st.is(11, "SI_UNIT") && st.is(11, "LENGTH_UNIT"));
    assert_eq!(st.rec(11, "SI_UNIT").unwrap()[0].en(), Some("MILLI"));
    assert_eq!(st.all("CARTESIAN_POINT"), &[10]);
    assert_eq!(st.rec(12, "MEASURE").unwrap()[2].id(), Some(10));
}

#[test]
fn duplicate_entity_number_is_an_error() {
    let text = b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=DIRECTION('',(1.,0.,0.));\n#1=DIRECTION('',(0.,1.,0.));\nENDSEC;\nEND-ISO-10303-21;\n";
    let e = parse(text, None).unwrap_err();
    assert!(e.contains("#1") && e.contains("twice"), "{e}");
}
