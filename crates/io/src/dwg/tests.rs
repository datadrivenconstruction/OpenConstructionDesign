// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;
use crate::dxf::{self, Entity};

/// Read-only corpus of .dwg files with reference .dxf exports (outside the repo; optional).
fn corpus() -> Vec<(PathBuf, PathBuf)> {
    let Ok(root) = std::env::var("DDC_DWG_CORPUS").map(PathBuf::from) else {
        return vec![];
    };
    let Ok(rd) = std::fs::read_dir(&root) else {
        return vec![];
    };
    let mut v: Vec<_> = rd
        .flatten()
        .map(|e| (e.path().join("input.dwg"), e.path().join("reference.dxf")))
        .filter(|(a, _)| a.is_file())
        .collect();
    v.sort();
    v
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(src: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut d = src.to_vec();
    match rng.below(4) {
        0 => {
            for _ in 0..1 + rng.below(16) {
                let i = rng.below(d.len());
                d[i] = rng.next() as u8;
            }
        }
        1 => d.truncate(rng.below(d.len())),
        2 => {
            let i = rng.below(d.len());
            let n = rng.below(64).min(d.len() - i);
            for b in &mut d[i..i + n] {
                *b = 0xFF;
            }
        }
        _ => {
            let i = 6 + rng.below(d.len().saturating_sub(6).min(0x200));
            if i < d.len() {
                d[i] ^= 1 << rng.below(8);
            }
        }
    }
    d
}

fn no_panic(d: &[u8]) {
    let r = std::panic::catch_unwind(|| {
        let _ = read_bytes_report(d);
    });
    assert!(r.is_ok(), "reader panicked");
}

#[test]
fn version_detection() {
    assert_eq!(version_of(b"AC1015\0\0").unwrap(), Version::R2000);
    assert_eq!(version_of(b"AC1032xx").unwrap(), Version::R2018);
    let e = version_of(b"AC1009\0\0").unwrap_err().to_string();
    assert!(e.contains("R12 or older"), "{e}");
    let e = version_of(b"AC2.10\0\0").unwrap_err().to_string();
    assert!(e.contains("not supported"), "{e}");
    assert!(version_of(b"AC1040\0\0")
        .unwrap_err()
        .to_string()
        .contains("AC1040"));
    assert!(version_of(b"0\nSECTION").is_err());
    assert!(is_dwg(b"AC1024") && !is_dwg(b"  0\nSECTION"));
    // Text that merely starts with "AC" is left to the DXF reader.
    assert!(!is_dwg(b"ACCESS log") && !is_dwg(b"AC10x5") && !is_dwg(b"AC1"));
    assert!(is_dwg(b"AC1.50") && is_dwg(b"AC2.10") && is_dwg(b"AC1009"));
    let e = version_of(b"AC1.40\0\0").unwrap_err().to_string();
    assert!(e.contains("R12 or older"), "{e}");
}

#[test]
fn legacy_r12_typed_error() {
    let mut d = b"AC1009".to_vec();
    d.resize(0x100, 0);
    d[0x14..0x18].copy_from_slice(&0x100u32.to_le_bytes());
    d[0x18..0x1C].copy_from_slice(&0x10Eu32.to_le_bytes());
    d.extend_from_slice(&[1, 0, 6, 0, 0xAA, 0xBB]);
    d.extend_from_slice(&[2, 0, 8, 0, 0, 0, 0xCC, 0xDD]);
    let e = read_bytes(&d).unwrap_err();
    assert_eq!(e.kind(), DwgErrorKind::Legacy);
    assert!(
        e.0.contains("2 entity records") && e.0.contains("R13 or newer"),
        "{e}"
    );
    // Inconsistent bounds: still the typed error, without the count.
    d[0x18..0x1C].copy_from_slice(&0x10Du32.to_le_bytes());
    let e = read_bytes(&d).unwrap_err();
    assert!(
        e.kind() == DwgErrorKind::Legacy && !e.0.contains("entity records"),
        "{e}"
    );
    assert_eq!(
        version_of(b"AC1040\0\0").unwrap_err().kind(),
        DwgErrorKind::Unsupported
    );
    assert_eq!(
        version_of(b"0\nSECTION").unwrap_err().kind(),
        DwgErrorKind::NotDwg
    );
}

#[test]
fn handle_map_chunk() {
    // One chunk: (handle +1, offset +16), (handle +2, offset +5), then the terminator chunk.
    let body = [0x01, 0x10, 0x02, 0x05];
    let size = (body.len() + 2) as u16;
    let mut d = size.to_be_bytes().to_vec();
    d.extend_from_slice(&body);
    let crc = bits::crc16(0xC0C1, &d);
    d.extend_from_slice(&crc.to_be_bytes());
    d.extend_from_slice(&[0, 2, 0, 0]);
    let mut rep = DwgReport::default();
    assert_eq!(objects::handle_map(&d, &mut rep), vec![(1, 16), (3, 21)]);
    assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for v in [
        "AC1012", "AC1014", "AC1015", "AC1018", "AC1021", "AC1024", "AC1027", "AC1032",
    ] {
        for len in [6usize, 32, 0x100, 0x500, 0x4000] {
            let mut d: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            d[..6].copy_from_slice(v.as_bytes());
            no_panic(&d);
        }
    }
}

/// 500 mutated corpus files (50 per seed); skipped when the corpus is absent.
#[test]
fn fuzz_corpus_mutations() {
    let files = corpus();
    if files.is_empty() {
        eprintln!("dwg corpus absent; skipped");
        return;
    }
    let mut seeds: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut by_ver: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    for (dwg, _) in &files {
        let Ok(d) = std::fs::read(dwg) else { continue };
        if d.len() > 400_000 || d.len() < 6 {
            continue;
        }
        let k = by_ver.entry(d[..6].to_vec()).or_default();
        *k += 1;
        if *k <= 2 && seeds.len() < 10 {
            seeds.insert([d[..6].to_vec(), k.to_le_bytes().to_vec()].concat(), d);
        }
    }
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut n = 0;
    while n < 500 {
        for d in seeds.values() {
            no_panic(&mutate(d, &mut rng));
            n += 1;
        }
        if seeds.is_empty() {
            break;
        }
    }
}

fn type_of(e: &Entity) -> Option<String> {
    Some(match e {
        Entity::Line(_) => "LINE".into(),
        Entity::LwPolyline(_) => "LWPOLYLINE".into(),
        Entity::Circle(_) => "CIRCLE".into(),
        Entity::Arc(_) => "ARC".into(),
        Entity::Text(_) => "TEXT".into(),
        Entity::MText(_) => "MTEXT".into(),
        Entity::Insert(_) => "INSERT".into(),
        Entity::AttDef(_) => "ATTDEF".into(),
        Entity::Unknown(v) => {
            let name = v.first()?.value.clone();
            if name == "POLYLINE" {
                let f: i32 = v
                    .iter()
                    .find(|p| p.code == 70)
                    .and_then(|p| p.value.trim().parse().ok())
                    .unwrap_or(0);
                if f & (8 | 16 | 64) == 0 {
                    return Some("LWPOLYLINE".into());
                }
            }
            if matches!(name.as_str(), "VERTEX" | "SEQEND" | "ATTRIB") {
                return None;
            }
            name
        }
    })
}

fn paper(e: &Entity) -> bool {
    let c = match e {
        Entity::Line(x) => &x.common,
        Entity::LwPolyline(x) => &x.common,
        Entity::Circle(x) => &x.common,
        Entity::Arc(x) => &x.common,
        Entity::Text(x) => &x.common,
        Entity::MText(x) => &x.common,
        Entity::Insert(x) => &x.common,
        Entity::AttDef(x) => &x.common,
        Entity::Unknown(v) => return v.iter().any(|p| p.code == 67 && p.value.trim() == "1"),
    };
    c.extra
        .iter()
        .any(|p| p.code == 67 && p.value.trim() == "1")
}

fn counts(ents: &[Entity]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for e in ents.iter().filter(|e| !paper(e)) {
        if let Some(t) = type_of(e) {
            *m.entry(t).or_default() += 1;
        }
    }
    m
}

/// The pair's identity check (`../verify01/<case>.json` next to the corpus) found the DXF
/// to be the same revision as the DWG (exact or near-exact handle set).
fn same_revision(dwg: &Path) -> bool {
    let Some(case) = dwg.parent() else {
        return false;
    };
    let (Some(name), Some(root)) = (case.file_name(), case.parent().and_then(Path::parent)) else {
        return false;
    };
    let file = root
        .join("verify01")
        .join(format!("{}.json", name.to_string_lossy()));
    std::fs::read_to_string(file).is_ok_and(|s| {
        s.contains("\"SAME_REVISION_EXACT_HANDLE_SET\"")
            || s.contains("\"SAME_DRAWING_NEAR_EXACT_HANDLES\"")
    })
}

fn hv(doc: &dxf::DxfDocument, name: &str, code: i32) -> Option<f64> {
    doc.header
        .iter()
        .find(|h| h.name == name)?
        .values
        .iter()
        .find(|p| p.code == code)?
        .value
        .trim()
        .parse()
        .ok()
}

/// Parity with the reference exports: per-type model-space entity counts, block count,
/// INSUNITS and EXTMIN. Prints a table; asserts only that nothing panics.
/// `cargo test --release -p ddc-io dwg::tests::corpus_parity -- --ignored --nocapture`
#[test]
#[ignore = "local corpus: DDC_DWG_CORPUS"]
fn corpus_parity() {
    let files = corpus();
    let mut per_ver: BTreeMap<String, [f64; 8]> = BTreeMap::new();
    let mut kinds: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    // Same per-type totals over identity-checked pairs only (the DXF is the same revision
    // as the DWG); other pairs are often a different revision or an exploded R12 export.
    let mut kinds_same: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut worst: Vec<(f64, String)> = vec![];
    let (mut bytes, mut secs) = (0usize, 0f64);
    // DDC_DWG_PARITY_OUT=<file>: per-file, per-kind TSV (case, ver, kind, ours, ref).
    let mut tsv = String::new();
    for (dwg, dxf_path) in &files {
        let raw = std::fs::read(dwg).unwrap_or_default();
        let ver = String::from_utf8_lossy(raw.get(..6).unwrap_or(b"?")).to_string();
        let row = per_ver.entry(ver.clone()).or_default();
        row[0] += 1.0;
        let t0 = std::time::Instant::now();
        let got = read_bytes_report(&raw);
        secs += t0.elapsed().as_secs_f64();
        bytes += raw.len();
        let Ok((doc, rep)) = got else {
            continue;
        };
        row[1] += 1.0;
        let Ok(reference) = dxf::read_file(dxf_path) else {
            continue;
        };
        row[2] += 1.0;
        let (a, b) = (counts(&doc.entities), counts(&reference.entities));
        let same = same_revision(dwg);
        let (mut mn, mut mx) = (0usize, 0usize);
        for k in a
            .keys()
            .chain(b.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            let (x, y) = (
                a.get(k).copied().unwrap_or(0),
                b.get(k).copied().unwrap_or(0),
            );
            mn += x.min(y);
            mx += x.max(y);
            let case = dwg.parent().and_then(Path::file_name).unwrap_or_default();
            tsv += &format!(
                "{}\t{ver}\t{k}\t{x}\t{y}\t{}\n",
                case.to_string_lossy(),
                same as u8
            );
            let e = kinds.entry(k.clone()).or_default();
            e.0 += x;
            e.1 += y;
            if same {
                let e = kinds_same.entry(k.clone()).or_default();
                e.0 += x;
                e.1 += y;
            }
        }
        let score = if mx == 0 { 1.0 } else { mn as f64 / mx as f64 };
        row[3] += score;
        if score >= 0.999 {
            row[4] += 1.0;
        }
        let nb = |d: &dxf::DxfDocument| {
            d.blocks
                .iter()
                .filter(|b| !b.name.starts_with("*Paper_Space"))
                .count()
        };
        if nb(&doc) == nb(&reference) {
            row[5] += 1.0;
        }
        if hv(&doc, "$INSUNITS", 70) == hv(&reference, "$INSUNITS", 70) {
            row[6] += 1.0;
        }
        let close = |n: &str| match (hv(&doc, n, 10), hv(&reference, n, 10)) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-6 * y.abs().max(1.0),
            (None, None) => true,
            _ => false,
        };
        if close("$EXTMIN") {
            row[7] += 1.0;
        }
        worst.push((
            score,
            format!(
                "{} {ver} ours={} ref={} failed={} skipped={:?}",
                dwg.parent()
                    .and_then(Path::file_name)
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                a.values().sum::<usize>(),
                b.values().sum::<usize>(),
                rep.failed,
                rep.skipped
            ),
        ));
    }
    println!("ver     files read refok  count%  exact blocks units extmin");
    let mut tot = [0f64; 8];
    for (v, r) in &per_ver {
        for i in 0..8 {
            tot[i] += r[i];
        }
        let pct = if r[2] > 0.0 { 100.0 * r[3] / r[2] } else { 0.0 };
        println!(
            "{v}  {:5} {:4} {:5}  {pct:6.2}  {:5} {:6} {:5} {:6}",
            r[0], r[1], r[2], r[4], r[5], r[6], r[7]
        );
    }
    let pct = if tot[2] > 0.0 {
        100.0 * tot[3] / tot[2]
    } else {
        0.0
    };
    println!(
        "TOTAL   {:5} {:4} {:5}  {pct:6.2}  {:5} {:6} {:5} {:6}",
        tot[0], tot[1], tot[2], tot[4], tot[5], tot[6], tot[7]
    );
    println!("kind: ours / reference");
    for (k, (x, y)) in &kinds {
        let (sx, sy) = kinds_same.get(k).copied().unwrap_or_default();
        println!("  {k:16} {x:8} {y:8}   same revision: {sx:8} {sy:8}");
    }
    worst.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (s, w) in worst.iter().take(15) {
        println!("  {s:.3} {w}");
    }
    if let Ok(out) = std::env::var("DDC_DWG_PARITY_OUT") {
        let _ = std::fs::write(out, tsv);
    }
    println!(
        "throughput: {:.1} MB in {secs:.2} s ({:.2} s per 20 MB)",
        bytes as f64 / 1e6,
        secs / (bytes as f64 / 20e6).max(1e-9)
    );
}

// ---- records as the DWG builder emits them go through the same import ------------------------

fn dwg_rec(kind: &str, pairs: Vec<(i32, String)>) -> Entity {
    let mut v = vec![dxf::Pair::new(0, kind), dxf::Pair::new(8, "L")];
    v.extend(pairs.into_iter().map(|(c, s)| dxf::Pair::new(c, s)));
    Entity::Unknown(v)
}

fn import_doc(ents: Vec<Entity>) -> (ddc_model::Model, dxf::ImportReport) {
    let doc = dxf::DxfDocument {
        entities: ents,
        ..Default::default()
    };
    let mut m = ddc_model::Model::open(":memory:").unwrap();
    let rep = dxf::import_into(&mut m, &doc).unwrap();
    (m, rep)
}

#[test]
fn dwg_spline_with_fit_points_imports_as_polyline() {
    let mut p = vec![
        (70, "1024".to_string()),
        (71, "3".into()),
        (72, "0".into()),
        (73, "0".into()),
        (74, "3".into()),
    ];
    for (x, y) in [(0, 0), (2, 1), (4, 0)] {
        p.extend([(11, x.to_string()), (21, y.to_string()), (31, "0".into())]);
    }
    let (m, rep) = import_doc(vec![dwg_rec("SPLINE", p)]);
    assert_eq!((rep.curves, rep.skipped_unknown), (1, 0));
    let v: i64 = m
        .sql()
        .query_row("SELECT count(*) FROM d_vertex", [], |r| r.get(0))
        .unwrap();
    assert!(v >= 3);
}

#[test]
fn dwg_ellipse_arc_imports_open_polyline() {
    let p = [
        (10, "0"),
        (20, "0"),
        (30, "0"),
        (11, "2"),
        (21, "0"),
        (31, "0"),
        (40, "0.5"),
        (41, "0"),
        (42, "3.141592653589793"),
    ]
    .map(|(c, s)| (c, s.to_string()))
    .to_vec();
    let (m, rep) = import_doc(vec![dwg_rec("ELLIPSE", p)]);
    assert_eq!(rep.curves, 1);
    let closed: i64 = m
        .sql()
        .query_row("SELECT closed FROM d_polyline", [], |r| r.get(0))
        .unwrap();
    assert_eq!(closed, 0);
}

#[test]
fn dwg_mtext_is_text_without_codes() {
    let t = dxf::MText {
        text: r"\A1;Hall\PNorth".into(),
        height: 2.0,
        ..Default::default()
    };
    let (m, rep) = import_doc(vec![Entity::MText(t)]);
    assert_eq!(rep.texts, 1);
    let c: String = m
        .sql()
        .query_row("SELECT content FROM d_text", [], |r| r.get(0))
        .unwrap();
    assert_eq!(c, "Hall\nNorth");
}

#[test]
fn dwg_unsupported_records_are_counted_by_kind() {
    let (_, rep) = import_doc(vec![
        dwg_rec("DIMENSION", vec![]),
        dwg_rec("LEADER", vec![]),
        dwg_rec("SOLID", vec![]),
        dwg_rec("ACAD_PROXY_ENTITY", vec![]),
    ]);
    assert_eq!(rep.skipped_unknown, 4);
    for k in ["dimension", "leader", "solid", "proxy"] {
        assert_eq!(rep.dropped.get(k), Some(&1), "{k}");
    }
}
