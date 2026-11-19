use super::*;

#[test]
fn comment_999_is_skipped() {
    let t = "999\nmade by a tool\n0\nSECTION\n2\nENTITIES\n999\ninner\n0\nLINE\n8\n0\n10\n0\n20\n0\n11\n1\n21\n0\n0\nENDSEC\n0\nEOF\n";
    let d = read_str(t).unwrap();
    assert_eq!(d.entities.len(), 1);
}

fn binary_header() -> Vec<u8> {
    let mut h = b"Sentinl ".to_vec();
    h.extend_from_slice(b"Binary DXF\r\n\x1a\0");
    h
}

#[test]
fn binary_dxf_header_is_rejected() {
    let h = binary_header();
    assert_eq!(h.len(), 22);
    assert!(super::reader::is_binary_dxf(&h));
    let e = read_bytes(&h).unwrap_err();
    assert!(e.0.contains("binary DXF"), "{}", e.0);
}

#[test]
fn text_and_truncated_headers_are_not_binary() {
    let text = b"  0\nSECTION\n  2\nENTITIES\n  0\nENDSEC\n  0\nEOF\n";
    assert!(!super::reader::is_binary_dxf(text));
    let h = binary_header();
    assert!(!super::reader::is_binary_dxf(&h[..21]));
    assert!(!super::reader::is_binary_dxf(&h[..8]));
    assert!(!super::reader::is_binary_dxf(b""));
    let mut bad = h.clone();
    bad[7] = b'x';
    assert!(!super::reader::is_binary_dxf(&bad));
    assert!(read_bytes(text).is_ok());
}

#[test]
fn dwg_signature_is_sniffed() {
    assert!(looks_like_dwg(b"AC1032\0\0\0\0"));
    assert!(looks_like_dwg(b"AC2.10xxxx"));
    assert!(!looks_like_dwg(b"  0\nSECTION\n"));
    assert!(!looks_like_dwg(b"AC"));
    assert_eq!(
        DWG_SUPPORTED,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/dwg/mod.rs")
            .exists()
    );
}

#[test]
fn block_record_kept_on_read_written_once() {
    let t = "0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nBLOCK_RECORD\n70\n1\n0\nBLOCK_RECORD\n5\n1F\n2\n*U3\n1001\nAcDbBlockRepBTag\n1070\n1\n1005\n2A\n0\nENDTAB\n0\nENDSEC\n0\nEOF\n";
    let d = read_str(t).unwrap();
    let br = d
        .other_tables
        .iter()
        .find(|t| t.name == "BLOCK_RECORD")
        .expect("kept for *U resolution");
    assert_eq!(br.entries.len(), 1);
    // the writer derives BLOCK_RECORD itself; the raw copy must not be written a second time
    let back = read_str(&write_string(&d)).unwrap();
    assert_eq!(
        back.other_tables
            .iter()
            .filter(|t| t.name == "BLOCK_RECORD")
            .count(),
        1
    );
}

fn sample() -> DxfDocument {
    let mut doc = DxfDocument::default();
    doc.header.push(HeaderVar {
        name: "$ACADVER".into(),
        values: vec![Pair::new(1, "AC1015")],
    });
    doc.header.push(HeaderVar {
        name: "$EXTMIN".into(),
        values: vec![
            Pair::new(10, "0.1"),
            Pair::new(20, "-2.5"),
            Pair::new(30, "0"),
        ],
    });
    doc.ltypes.push(LineType {
        name: "DASHED".into(),
        description: "dashes".into(),
        flags: 0,
        pattern_length: 0.75,
        pattern: vec![0.5, -0.25],
        extra: vec![],
    });
    for (i, n) in ["0", "WALLS", "TEXT"].iter().enumerate() {
        doc.layers.push(Layer {
            name: (*n).into(),
            flags: 0,
            color: 1 + i as i32,
            linetype: "CONTINUOUS".into(),
            extra: vec![],
        });
    }
    doc.styles.push(TextStyle {
        name: "STANDARD".into(),
        flags: 0,
        height: 0.0,
        width_factor: 1.0,
        oblique: 0.0,
        font: "txt".into(),
        big_font: String::new(),
        extra: vec![],
    });
    let mut b = Block::new("DOOR");
    b.attdefs.push(AttDef {
        tag: "MARK".into(),
        prompt: "Mark".into(),
        default: "D?".into(),
        height: 2.5,
        ..Default::default()
    });
    b.entities.push(Entity::Line(Line {
        end: [0.9, 0.0, 0.0],
        ..Default::default()
    }));
    b.entities.push(Entity::Arc(Arc {
        radius: 0.9,
        end_angle: 90.0,
        ..Default::default()
    }));
    doc.blocks.push(b);

    for i in 0..100 {
        let f = i as f64;
        doc.entities.push(Entity::Line(Line {
            common: Common::on_layer("WALLS"),
            start: [f * 0.1 + 1.0 / 3.0, -f, 0.0],
            end: [f * 1e-7, 2.0f64.sqrt() * f, 0.0],
        }));
    }
    doc.entities.push(Entity::LwPolyline(LwPolyline {
        common: Common::on_layer("WALLS"),
        flags: 1,
        vertices: vec![
            LwVertex {
                x: 0.0,
                y: 0.0,
                bulge: 0.41421356237309503,
                ..Default::default()
            },
            LwVertex {
                x: 10.0,
                y: 0.0,
                ..Default::default()
            },
            LwVertex {
                x: 10.0,
                y: 5.5,
                bulge: -1.0,
                start_width: 0.1,
                end_width: 0.2,
            },
        ],
    }));
    doc.entities.push(Entity::Circle(Circle {
        center: [1.0, 2.0, 3.0],
        radius: 4.5,
        ..Default::default()
    }));
    doc.entities.push(Entity::Text(Text {
        common: Common::on_layer("TEXT"),
        pos: [5.0, 5.0, 0.0],
        height: 2.5,
        text: "Hello  world ".into(),
        rotation: 30.0,
        style: Some("STANDARD".into()),
    }));
    doc.entities.push(Entity::MText(MText {
        height: 2.0,
        width: 40.0,
        text: "x".repeat(600),
        ..Default::default()
    }));
    doc.entities.push(Entity::Insert(Insert {
        common: Common::on_layer("WALLS"),
        block: "DOOR".into(),
        pos: [3.0, 4.0, 0.0],
        scale: [1.0, 2.0, 1.0],
        rotation: 45.0,
        attribs: vec![Attrib {
            tag: "MARK".into(),
            value: "D12".into(),
            height: 2.5,
            ..Default::default()
        }],
    }));
    doc.entities.push(Entity::Unknown(vec![
        Pair::new(0, "SPLINE"),
        Pair::new(8, "0"),
        Pair::new(71, "3"),
        Pair::new(10, "1.5"),
        Pair::new(20, "2.5"),
    ]));
    doc.other_tables.push(RawTable {
        name: "APPID".into(),
        header: vec![Pair::new(70, "1")],
        entries: vec![vec![Pair::new(0, "APPID"), Pair::new(2, "ACAD")]],
    });
    doc.other_sections.push(RawSection {
        name: "OBJECTS".into(),
        pairs: vec![Pair::new(0, "DICTIONARY"), Pair::new(5, "C")],
    });
    doc
}

#[test]
fn roundtrip_document() {
    let doc = sample();
    let text = write_string(&doc);
    let back = read_str(&text).unwrap();
    // Header, linetype table and OBJECTS gain mandatory R2000+ defaults on write.
    assert_eq!(back.layers, doc.layers);
    assert_eq!(back.styles, doc.styles);
    assert_eq!(back.blocks, doc.blocks);
    assert_eq!(back.entities.len(), doc.entities.len());
    assert!(back.ltypes.iter().any(|t| t == &doc.ltypes[0]));
    assert_eq!(back.layers.len(), 3);
    assert_eq!(back.entities.len(), doc.entities.len());
    for (a, b) in doc.entities.iter().zip(&back.entities) {
        if let (Entity::Line(a), Entity::Line(b)) = (a, b) {
            for k in 0..3 {
                assert!((a.start[k] - b.start[k]).abs() <= 1e-9);
                assert!((a.end[k] - b.end[k]).abs() <= 1e-9);
            }
        }
    }
    let Some(Entity::Insert(ins)) = back
        .entities
        .iter()
        .find(|e| matches!(e, Entity::Insert(_)))
    else {
        panic!("no insert")
    };
    assert_eq!(ins.attribs[0].tag, "MARK");
    assert_eq!(ins.attribs[0].value, "D12");
    assert_eq!(back.blocks[0].attdefs[0].tag, "MARK");
    assert_eq!(read_str(&write_string(&back)).unwrap().layers, back.layers);
}

#[test]
fn unknown_entity_survives() {
    let doc = sample();
    let back = read_str(&write_string(&doc)).unwrap();
    assert!(back.entities.iter().any(|e| matches!(
        e,
        Entity::Unknown(p) if p[0].value == "SPLINE" && p.len() >= 5 && p.contains(&Pair::new(10, "1.5"))
    )));
    assert_eq!(back.other_sections[0].name, "OBJECTS");
}

#[test]
fn parse_handwritten_fixture_crlf() {
    let src = "0\nSECTION\n2\nHEADER\n9\n$ACADVER\n1\nAC1009\n0\nENDSEC\n\
0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nLAYER\n70\n1\n0\nLAYER\n2\nA-WALL\n70\n0\n62\n3\n6\nCONTINUOUS\n0\nENDTAB\n0\nENDSEC\n\
0\nSECTION\n2\nENTITIES\n\
0\nLINE\n8\nA-WALL\n10\n0.0\n20\n0.0\n30\n0.0\n11\n10.0\n21\n5.0\n31\n0.0\n\
0\nLWPOLYLINE\n8\nA-WALL\n90\n2\n70\n0\n10\n0\n20\n0\n42\n0.5\n10\n1\n20\n1\n\
0\nINSERT\n8\n0\n66\n1\n2\nB\n10\n1\n20\n2\n\
0\nATTRIB\n8\n0\n10\n1\n20\n2\n40\n1\n1\nV1\n2\nT1\n70\n0\n\
0\nSEQEND\n8\n0\n\
0\nCIRCLE\n8\n0\n5\nAB\n10\n1\n20\n1\n40\n2\n\
0\nENDSEC\n0\nEOF\n"
        .replace('\n', "\r\n");
    let d = read_str(&src).unwrap();
    assert_eq!(d.header[0].values[0].value, "AC1009");
    assert_eq!(d.layers[0].name, "A-WALL");
    assert_eq!(d.layers[0].color, 3);
    assert_eq!(d.entities.len(), 4);
    match &d.entities[0] {
        Entity::Line(l) => assert_eq!(l.end, [10.0, 5.0, 0.0]),
        e => panic!("{e:?}"),
    }
    match &d.entities[1] {
        Entity::LwPolyline(p) => {
            assert_eq!(p.vertices.len(), 2);
            assert_eq!(p.vertices[0].bulge, 0.5);
        }
        e => panic!("{e:?}"),
    }
    match &d.entities[2] {
        Entity::Insert(i) => assert_eq!(i.attribs[0].value, "V1"),
        e => panic!("{e:?}"),
    }
    match &d.entities[3] {
        Entity::Circle(c) => assert!(c.common.extra.is_empty()),
        e => panic!("{e:?}"),
    }
    assert_eq!(read_str(&write_string(&d)).unwrap().entities, d.entities);
}

#[test]
fn bad_input_errors() {
    assert!(read_str("0\nSECTION\n2\nENTITIES\n").is_err());
    assert!(read_str("x\ny\n").is_err());
}

fn mtext(t: &str) -> DxfDocument {
    let mut d = DxfDocument::default();
    d.entities.push(Entity::MText(MText {
        text: t.into(),
        ..Default::default()
    }));
    d
}

#[test]
fn newlines_never_break_pairs() {
    let text = write_string(&mtext("A\nB\r\nC\rD"));
    let back = read_str(&text).unwrap();
    let Entity::MText(m) = &back.entities[0] else {
        panic!()
    };
    assert_eq!(m.text, r"A\PB\PC\PD");
    // other string fields: newline becomes a space
    let mut d = DxfDocument::default();
    d.layers.push(Layer {
        name: "L\nX".into(),
        flags: 0,
        color: 7,
        linetype: "CONTINUOUS".into(),
        extra: vec![],
    });
    d.entities.push(Entity::Text(Text {
        text: "a\nb".into(),
        ..Default::default()
    }));
    let back = read_str(&write_string(&d)).unwrap();
    assert!(back.layers.iter().any(|l| l.name == "L X"));
    let Entity::Text(t) = &back.entities[0] else {
        panic!()
    };
    assert_eq!(t.text, "a b");
}

#[test]
fn cp1251_layer_name_decoded() {
    let mut bytes = b"0\nSECTION\n2\nHEADER\n9\n$DWGCODEPAGE\n3\nANSI_1251\n0\nENDSEC\n\
0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nLAYER\n70\n1\n0\nLAYER\n2\n"
        .to_vec();
    bytes.extend_from_slice(&[0xD1, 0xF2, 0xE5, 0xED, 0xFB]);
    bytes.extend_from_slice(b"\n70\n0\n62\n3\n6\nCONTINUOUS\n0\nENDTAB\n0\nENDSEC\n0\nEOF\n");
    let d = read_bytes(&bytes).unwrap();
    assert_eq!(d.layers[0].name, crate::ru_fixtures::ru("walls_layer"));
    // valid UTF-8 wins over the declared page
    let utf8 = String::from_utf8_lossy(&bytes).replace('\u{FFFD}', "");
    assert!(read_bytes(utf8.as_bytes()).is_ok());
    let mut u = b"0\nSECTION\n2\nHEADER\n9\n$DWGCODEPAGE\n3\nANSI_1251\n0\nENDSEC\n0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nLAYER\n0\nLAYER\n2\n".to_vec();
    u.extend_from_slice(crate::ru_fixtures::ru("walls_layer").as_bytes());
    u.extend_from_slice(b"\n0\nENDTAB\n0\nENDSEC\n0\nEOF\n");
    assert_eq!(read_bytes(&u).unwrap().layers[0].name, crate::ru_fixtures::ru("walls_layer"));
    assert_eq!(
        codepage::decode(&[0xE9, 0x80], codepage::page("ansi_1252")),
        "é€"
    );
    assert_eq!(codepage::decode(&[0xE1], codepage::page("ANSI_1253")), "α");
}

#[test]
fn non_finite_numbers_rejected() {
    for bad in ["nan", "inf", "-inf", "NaN", "infinity"] {
        let src = format!(
            "0\nSECTION\n2\nENTITIES\n0\nLINE\n8\n0\n10\n{bad}\n20\n0\n0\nENDSEC\n0\nEOF\n"
        );
        let e = read_str(&src).unwrap_err();
        assert!(e.0.contains("bad number at line"), "{bad}: {e}");
    }
}

fn structure(text: &str) -> (Vec<String>, Vec<String>, u64, Vec<(String, String)>) {
    // handles (code 5/105 only in records), seed, BLOCK names and BLOCK_RECORD names
    let lines: Vec<&str> = text.lines().collect();
    let (mut handles, mut seed, mut blocks, mut recs) = (vec![], 0u64, vec![], vec![]);
    let (mut sec, mut kind) = (String::new(), String::new());
    let mut i = 0;
    while i + 1 < lines.len() {
        let (c, v) = (lines[i].trim(), lines[i + 1]);
        match c {
            "0" => kind = v.to_string(),
            "2" if kind == "SECTION" => sec = v.to_string(),
            "5" | "105" if sec != "HEADER" => handles.push(v.to_string()),
            "5" => seed = u64::from_str_radix(v, 16).unwrap(),
            "2" if kind == "BLOCK" => blocks.push(v.to_string()),
            "2" if kind == "BLOCK_RECORD" => recs.push((v.to_string(), String::new())),
            _ => {}
        }
        i += 2;
    }
    (handles, blocks, seed, recs)
}

#[test]
fn writer_emits_consistent_r2000_structure() {
    let doc = sample();
    let text = write_string(&doc);
    assert!(text.contains("\n$ACADVER\n1\nAC10") || text.contains("\n$ACADVER\n1\nAC1021"));
    let (handles, blocks, seed, recs) = structure(&text);
    let mut set = std::collections::HashSet::new();
    let mut max = 0;
    for h in &handles {
        assert!(set.insert(h.clone()), "duplicate handle {h}");
        max = max.max(u64::from_str_radix(h, 16).unwrap());
    }
    assert!(seed > max, "HANDSEED {seed:X} <= {max:X}");
    assert!(handles.len() > 100);
    for b in &blocks {
        assert!(
            recs.iter().any(|(n, _)| n.eq_ignore_ascii_case(b)),
            "no BLOCK_RECORD for {b}"
        );
    }
    for n in ["*Model_Space", "*Paper_Space", "DOOR"] {
        assert!(blocks.iter().any(|b| b == n), "no BLOCK {n}");
        assert!(recs.iter().any(|(r, _)| r == n), "no record {n}");
    }
    // every owner (330) other than 0 refers to an existing handle
    let lines: Vec<&str> = text.lines().collect();
    for w in lines.chunks(2) {
        if w[0].trim() == "330" && w[1] != "0" {
            assert!(set.contains(w[1]), "dangling owner {}", w[1]);
        }
    }
    // reading it back keeps working and the second write is still consistent
    let again = write_string(&read_str(&text).unwrap());
    let (h2, _, s2, _) = structure(&again);
    let uniq: std::collections::HashSet<_> = h2.iter().collect();
    assert_eq!(uniq.len(), h2.len());
    assert!(
        s2 > h2
            .iter()
            .map(|h| u64::from_str_radix(h, 16).unwrap())
            .max()
            .unwrap()
    );
}

#[test]
fn objects_references_never_dangle() {
    let mut d = DxfDocument::default();
    d.layers.push(Layer {
        name: "L".into(),
        flags: 0,
        color: 7,
        linetype: "CONTINUOUS".into(),
        extra: vec![],
    });
    let p = Pair::new;
    d.other_sections.push(RawSection {
        name: "OBJECTS".into(),
        pairs: vec![
            p(0, "DICTIONARY"),
            p(5, "A0"),
            p(330, "0"),
            p(100, "AcDbDictionary"),
            p(3, "GOOD"),
            p(350, "A1"),
            p(3, "STALE"),
            p(350, "2"), // old modeled handle: regenerated on write
            p(3, "STALE2"),
            p(360, "FFFF"),
            p(0, "XRECORD"),
            p(5, "A1"),
            p(102, "{ACAD_REACTORS"),
            p(330, "A0"),
            p(330, "7777"),
            p(102, "}"),
            p(330, "9999"),
            p(340, "A0"),
            p(340, "8888"),
        ],
    });
    let text = write_string(&d);
    let back = read_str(&text).unwrap();
    let obj = &back
        .other_sections
        .iter()
        .find(|s| s.name == "OBJECTS")
        .unwrap()
        .pairs;
    let handles: std::collections::HashSet<u64> = text_pairs(&text)
        .iter()
        .filter(|q| q.code == 5 || q.code == 105)
        .map(|q| u64::from_str_radix(q.value.trim(), 16).unwrap())
        .collect();
    for q in obj.iter().filter(|q| (330..=369).contains(&q.code)) {
        let h = u64::from_str_radix(q.value.trim(), 16).unwrap();
        assert!(
            h == 0 || handles.contains(&h),
            "dangling {} {}",
            q.code,
            q.value
        );
    }
    // Live references are kept; names of dead dictionary entries went away with them.
    assert!(obj.iter().any(|q| q.code == 3 && q.value == "GOOD"));
    assert!(!obj
        .iter()
        .any(|q| q.code == 3 && q.value.starts_with("STALE")));
    assert_eq!(obj.iter().filter(|q| q.code == 340).count(), 1);
}

fn text_pairs(text: &str) -> Vec<Pair> {
    let lines: Vec<&str> = text.lines().collect();
    lines
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| Pair::new(c[0].trim().parse().unwrap(), c[1]))
        .collect()
}

#[test]
fn stop_check_cancels_the_reader() {
    let text = "0\nSECTION\n2\nENTITIES\n0\nENDSEC\n0\nEOF\n";
    assert!(read_str_with(text, &|| false).is_ok());
    let e = read_str_with(text, &|| true).unwrap_err();
    assert_eq!(e.0, crate::job::CANCELLED);
    let dir = std::env::temp_dir().join(format!("ddc-o3-dxf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("a.dxf");
    std::fs::write(&f, text).unwrap();
    assert!(read_file_with(&f, &|| false).is_ok());
    assert_eq!(
        read_file_with(&f, &|| true).unwrap_err().0,
        crate::job::CANCELLED
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_read_reports_bytes_then_sections() {
    let text = "0\nSECTION\n2\nHEADER\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n0\nLINE\n8\n0\n\
                10\n0\n20\n0\n11\n1\n21\n0\n0\nENDSEC\n0\nEOF\n";
    let dir = std::env::temp_dir().join(format!("ddc-imp1-dxf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("p.dxf");
    std::fs::write(&f, text).unwrap();
    let c = crate::job::Progress::new();
    let d = read_file_ctl(&f, &c).unwrap();
    assert_eq!(d.entities.len(), 1);
    assert_eq!(c.trail(), vec!["job.stage.read", "job.stage.parse"]);
    let (done, total) = c.counts();
    assert!(total > 0 && done == total, "{done} of {total}");
    assert!((c.fraction() - 1.0).abs() < 1e-6);
    assert_eq!(c.detail(), "job.kind.entities");
    let stop = crate::job::Progress::new();
    stop.cancel();
    assert_eq!(read_file_ctl(&f, &stop).unwrap_err().0, crate::job::CANCELLED);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- import: dropped entities by kind, curves and MTEXT --------------------------------------

fn raw(kind: &str, pairs: &[(i32, &str)]) -> Entity {
    let mut v = vec![Pair::new(0, kind), Pair::new(8, "L")];
    v.extend(pairs.iter().map(|(c, s)| Pair::new(*c, *s)));
    Entity::Unknown(v)
}

fn import_one(doc: &DxfDocument) -> (ddc_model::Model, ImportReport) {
    let mut m = ddc_model::Model::open(":memory:").unwrap();
    let rep = import_into(&mut m, doc).unwrap();
    (m, rep)
}

fn n(m: &ddc_model::Model, sql: &str) -> i64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn with(ents: Vec<Entity>) -> DxfDocument {
    DxfDocument {
        entities: ents,
        ..Default::default()
    }
}

fn ellipse(major: &str) -> Entity {
    raw(
        "ELLIPSE",
        &[
            (10, "0"),
            (20, "0"),
            (30, "0"),
            (11, major),
            (21, "0"),
            (31, "0"),
            (40, "0.5"),
            (41, "0"),
            (42, "6.283185307179586"),
        ],
    )
}

#[test]
fn import_spline_becomes_editable_polyline() {
    let s = raw(
        "SPLINE",
        &[
            (70, "8"),
            (71, "2"),
            (40, "0"),
            (40, "0"),
            (40, "0"),
            (40, "1"),
            (40, "1"),
            (40, "1"),
            (10, "0"),
            (20, "0"),
            (30, "0"),
            (10, "1"),
            (20, "2"),
            (30, "0"),
            (10, "2"),
            (20, "0"),
            (30, "0"),
        ],
    );
    let (m, rep) = import_one(&with(vec![s]));
    assert_eq!((rep.curves, rep.skipped_unknown, rep.polylines), (1, 0, 1));
    assert!(rep.dropped.is_empty());
    assert!(n(&m, "SELECT count(*) FROM d_vertex") > 4);
}

#[test]
fn import_ellipse_becomes_closed_polyline() {
    let (m, rep) = import_one(&with(vec![ellipse("3")]));
    assert_eq!((rep.curves, rep.skipped_unknown), (1, 0));
    assert_eq!(n(&m, "SELECT closed FROM d_polyline"), 1);
    assert!(n(&m, "SELECT count(*) FROM d_vertex") >= 60);
}

#[test]
fn curve_sag_follows_drawing_units() {
    let big = ellipse("10000");
    let plain = import_one(&with(vec![big.clone()])).0;
    let mut mm = with(vec![big]);
    mm.header = vec![HeaderVar {
        name: "$INSUNITS".into(),
        values: vec![Pair::new(70, "4")],
    }];
    let (fine, _) = import_one(&mm);
    let (a, b) = (
        n(&plain, "SELECT count(*) FROM d_vertex"),
        n(&fine, "SELECT count(*) FROM d_vertex"),
    );
    assert!(b > 2 * a, "{b} vs {a}");
}

#[test]
fn import_mtext_strips_codes_keeps_breaks() {
    let t = MText {
        text: r"{\fArial|b0;Room}\P\C1;12 m\~2".into(),
        height: 2.5,
        ..Default::default()
    };
    let (m, rep) = import_one(&with(vec![Entity::MText(t)]));
    assert_eq!(rep.texts, 1);
    let c: String = m
        .sql()
        .query_row("SELECT content FROM d_text", [], |r| r.get(0))
        .unwrap();
    assert_eq!(c, "Room\n12 m 2");
}

#[test]
fn import_reports_dropped_by_kind() {
    let doc = with(vec![
        raw("SPLINE", &[(70, "0")]),
        raw("ELLIPSE", &[(40, "0")]),
        raw("DIMENSION", &[(2, "")]),
        raw("LEADER", &[]),
        raw("3DSOLID", &[]),
        raw("SOLID", &[]),
        raw("ACAD_PROXY_ENTITY", &[]),
        raw("WIPEOUT", &[]),
    ]);
    let (_, rep) = import_one(&doc);
    assert_eq!(rep.skipped_unknown, 8);
    let got: Vec<(&str, usize)> = rep.dropped.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(
        got,
        vec![
            ("dimension", 1),
            ("ellipse", 1),
            ("leader", 1),
            ("others", 1),
            ("proxy", 1),
            ("solid", 2),
            ("spline", 1),
        ]
    );
    assert_eq!(dropped_kind("MTEXT"), "mtext");
}

#[test]
fn dbcs_gbk_936() {
    let p = codepage::page("ANSI_936");
    assert_eq!(codepage::decode(&[0xD6, 0xD0, 0xCE, 0xC4], p), "\u{4E2D}\u{6587}");
}

#[test]
fn dbcs_shift_jis_932() {
    let p = codepage::page("ANSI_932");
    let b = [0x83, 0x8C, 0x83, 0x43, 0x83, 0x84, b'_', b'1'];
    assert_eq!(codepage::decode(&b, p), "\u{30EC}\u{30A4}\u{30E4}_1");
}

#[test]
fn dbcs_big5_950() {
    let p = codepage::page("ANSI_950");
    assert_eq!(codepage::decode(&[0xA4, 0xA4, 0xA4, 0xE5], p), "\u{4E2D}\u{6587}");
}

#[test]
fn dbcs_euc_kr_949() {
    let p = codepage::page("ANSI_949");
    assert_eq!(codepage::decode(&[0xC7, 0xD1, 0xB1, 0xDB], p), "\u{D55C}\u{AE00}");
}

#[test]
fn dbcs_invalid_does_not_panic() {
    for n in ["ANSI_932", "ANSI_936", "ANSI_949", "ANSI_950"] {
        let s = codepage::decode(&[b'A', 0xFF, 0xD6], codepage::page(n));
        assert!(s.starts_with('A') && s.contains('\u{FFFD}'));
    }
}

#[test]
fn unicode_escape_plain_and_mixed() {
    assert_eq!(super::text::decode_unicode(r"\U+30EC"), "\u{30EC}");
    assert_eq!(super::text::decode_unicode(r"A-\U+30EC_2"), "A-\u{30EC}_2");
}
