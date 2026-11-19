use super::*;
use crate::pdf::{export_pdf, Item, Sheet, SheetLayer};
use std::f64::consts::PI;
use std::io::Write;

/// The scale the exporter writes into `cm` (6 decimals), in import mm.
const WRITER_K: f64 = 2.834646 * 25.4 / 72.0;

fn stream(extra: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {extra} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream");
    v
}

/// A file with a classic xref table; `objs[i]` is the body of object i+1, the catalog is object 1.
fn pdf(objs: &[Vec<u8>], trailer_extra: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = vec![];
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        write!(out, "{} 0 obj\n", i + 1).unwrap();
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let x = out.len();
    write!(out, "xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).unwrap();
    for o in offs {
        write!(out, "{o:010} 00000 n \n").unwrap();
    }
    write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R {trailer_extra} >>\nstartxref\n{x}\n%%EOF\n",
        objs.len() + 1
    )
    .unwrap();
    out
}

/// One A4 page (595×842) with content and resources.
fn one_page(content: &[u8], resources: &str, page_extra: &str, more: &[Vec<u8>]) -> Vec<u8> {
    let mut objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 595 842] >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources {resources} {page_extra} >>"
        )
        .into_bytes(),
        stream("", content),
    ];
    objs.extend_from_slice(more);
    pdf(&objs, "")
}

const K: f64 = 25.4 / 72.0;

fn near(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} vs {b} (tol {tol})");
}

fn sheet() -> Sheet {
    let poly = |pts: Vec<[f64; 2]>, closed, layer| Item::Polyline {
        pts,
        closed,
        weight_mm: 0.35,
        rgb: [200, 10, 30],
        layer,
    };
    Sheet {
        width_mm: 420.0,
        height_mm: 297.0,
        layers: vec![
            SheetLayer {
                name: "A-WALL".into(),
                visible: true,
            },
            SheetLayer {
                name: crate::ru_fixtures::ru("texts_layer").into(),
                visible: false,
            },
        ],
        items: vec![
            poly(vec![[10.0, 20.0], [110.125, 20.0]], false, 0),
            poly(
                vec![[0.5, 0.5], [50.0, 0.5], [50.0, 40.25], [0.5, 40.25]],
                true,
                0,
            ),
            poly(vec![[5.0, 5.0], [6.0, 7.0], [9.0, 2.0]], false, 1),
            Item::Text {
                pos: [30.0, 60.0],
                height_mm: 3.5,
                rotation: 30.0,
                string: crate::ru_fixtures::ru("wall_200mm").into(),
                rgb: [0, 0, 255],
                layer: 1,
            },
            Item::Text {
                pos: [300.0, 10.0],
                height_mm: 5.0,
                rotation: 0.0,
                string: format!("A-101 ({})", crate::ru_fixtures::ru("plan_word")),
                rgb: [0, 0, 0],
                layer: 0,
            },
        ],
    }
}

#[test]
fn roundtrip_own_export() {
    let sh = sheet();
    let bytes = export_pdf(std::slice::from_ref(&sh)).unwrap();
    let f = PdfFile::parse(&bytes).unwrap();
    assert_eq!(f.page_count(), 1);
    let (w, h) = f.page_size_mm(0).unwrap();
    near(w, 420.0, 2e-3);
    near(h, 297.0, 2e-3);
    let page = f.page(0).unwrap();
    assert!(!page.truncated);
    let ents = page.entities(&CurveOptions::default(), true);
    let lname = |e: &Entity| page.layers[e.layer].as_str();
    let tol = 1e-6;
    let mut lines = ents.iter().filter(|e| !matches!(e.geom, Geom::Text { .. }));
    // Export order: by layer, within a layer by item order.
    for it in sh
        .items
        .iter()
        .filter(|i| matches!(i, Item::Polyline { .. }))
    {
        let Item::Polyline {
            pts, closed, layer, ..
        } = it
        else {
            unreachable!()
        };
        let e = lines.next().unwrap();
        assert_eq!(lname(e), sh.layers[*layer].name);
        assert_eq!(e.rgb, [200, 10, 30]);
        near(e.width_mm, 0.35 * WRITER_K, tol);
        let got: Vec<P> = match &e.geom {
            Geom::Line { a, b } => {
                assert_eq!(pts.len(), 2);
                vec![*a, *b]
            }
            Geom::Polyline {
                pts: p,
                closed: c,
                bulges,
            } => {
                assert_eq!(c, closed);
                assert!(bulges.iter().all(|b| *b == 0.0));
                p.clone()
            }
            g => panic!("{g:?}"),
        };
        assert_eq!(got.len(), pts.len());
        for (g, p) in got.iter().zip(pts) {
            near(g[0], p[0] * WRITER_K, tol);
            near(g[1], p[1] * WRITER_K, tol);
        }
    }
    let texts: Vec<_> = ents
        .iter()
        .filter_map(|e| match &e.geom {
            Geom::Text {
                pos,
                height,
                rotation,
                string,
            } => Some((lname(e), *pos, *height, *rotation, string.as_str(), e.rgb)),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 2);
    let a = texts.iter().find(|t| t.4 == crate::ru_fixtures::ru("wall_200mm")).unwrap();
    assert_eq!(a.0, crate::ru_fixtures::ru("texts_layer"));
    assert_eq!(a.5, [0, 0, 255]);
    near(a.1[0], 30.0 * WRITER_K, tol);
    near(a.1[1], 60.0 * WRITER_K, tol);
    // Export writes cos/sin with 3 decimals: the height carries a factor |(0.866, 0.5)|.
    near(a.2, 3.5 * WRITER_K * 0.866f64.hypot(0.5), tol);
    // Export writes cos/sin with 3 decimals.
    near(a.3, 30f64.to_radians(), 1e-3);
    let plan_text = format!("A-101 ({})", crate::ru_fixtures::ru("plan_word"));
    let b = texts.iter().find(|t| t.4 == plan_text).unwrap();
    assert_eq!(b.0, "A-WALL");
    near(b.1[0], 300.0 * WRITER_K, tol);
    near(b.3, 0.0, 1e-12);
}

#[test]
fn ocg_layers_and_form_xobject() {
    // Layer via OCMD, a form with its own /OC, a nested form inherits the layer.
    let content = b"/OC /P0 BDC 0 0 m 72 0 l S EMC /Fm1 Do 1 0 0 1 100 0 cm /Fm2 Do";
    let form1 = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 500 500] /OC 7 0 R /Matrix [1 0 0 1 0 72] \
         /Resources << /XObject << /Fm2 6 0 R >> >>",
        b"0 0 m 0 72 l S /Fm2 Do",
    );
    let form2 = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 9 9]",
        b"0 0 72 72 re f",
    );
    let objs = vec![
        form1,
        form2,
        b"<< /Type /OCG /Name (Walls) >>".to_vec(),
        b"<< /Type /OCMD /OCGs [9 0 R] >>".to_vec(),
        b"<< /Type /OCG /Name <FEFF0421043B043E0439> >>".to_vec(),
    ];
    let res = "<< /Properties << /P0 8 0 R >> /XObject << /Fm1 5 0 R /Fm2 6 0 R >> >>";
    let f = PdfFile::parse(&one_page(content, res, "", &objs)).unwrap();
    let page = f.page(0).unwrap();
    let names: Vec<&str> = page.layers.iter().map(|s| s.as_str()).collect();
    assert_eq!(names, [crate::ru_fixtures::ru("layer_word"), "Walls", "PDF"]);
    let ents = page.entities(&CurveOptions::default(), true);
    let layer_of = |i: usize| page.layers[ents[i].layer].as_str();
    assert_eq!(ents.len(), 4);
    assert_eq!(layer_of(0), crate::ru_fixtures::ru("layer_word"));
    assert_eq!(layer_of(1), "Walls");
    assert_eq!(layer_of(2), "Walls");
    assert_eq!(layer_of(3), "PDF");
    // Form 1 is shifted 72 pt = 25.4 mm up.
    let Geom::Line { a, b } = &ents[1].geom else {
        panic!()
    };
    near(a[1], 25.4, 1e-9);
    near(b[1], 50.8, 1e-9);
    let Geom::Polyline { pts, closed, .. } = &ents[3].geom else {
        panic!()
    };
    assert!(closed);
    near(pts[0][0], 100.0 * K, 1e-9);
    assert!(ents[3].filled);
}

#[test]
fn xref_stream_and_object_stream() {
    // Objects 1 and 2 are in object stream 5, the xref is stream 6 (/W [1 2 1]), PNG predictor.
    let content = b"1 0 0 RG 2 w 0 0 m 72 144 l S";
    let hdr_objs = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    ];
    let mut body = String::new();
    let mut header = String::new();
    for (i, o) in hdr_objs.iter().enumerate() {
        header.push_str(&format!("{} {} ", i + 1, body.len()));
        body.push_str(o);
        body.push(' ');
    }
    let objstm_data = format!("{header}{body}");
    let first = header.len();
    let mut out = b"%PDF-1.5\n".to_vec();
    let mut offs = [0usize; 7];
    let mut put = |out: &mut Vec<u8>, n: usize, bytes: Vec<u8>| {
        offs[n] = out.len();
        write!(out, "{n} 0 obj\n").unwrap();
        out.extend_from_slice(&bytes);
        out.extend_from_slice(b"\nendobj\n");
    };
    put(
        &mut out,
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec(),
    );
    let z = miniz_oxide::deflate::compress_to_vec_zlib(content, 6);
    put(&mut out, 4, stream("/Filter /FlateDecode", &z));
    put(
        &mut out,
        5,
        stream(
            &format!("/Type /ObjStm /N 2 /First {first}"),
            objstm_data.as_bytes(),
        ),
    );
    offs[6] = out.len();
    let rows: Vec<[u8; 4]> = vec![
        [0, 0, 0, 255],
        [2, 0, 5, 0],
        [2, 0, 5, 1],
        [1, (offs[3] >> 8) as u8, offs[3] as u8, 0],
        [1, (offs[4] >> 8) as u8, offs[4] as u8, 0],
        [1, (offs[5] >> 8) as u8, offs[5] as u8, 0],
        [1, (offs[6] >> 8) as u8, offs[6] as u8, 0],
    ];
    // PNG Up: each row is the difference from the previous one.
    let mut raw = vec![];
    let mut prev = [0u8; 4];
    for r in &rows {
        raw.push(2);
        for i in 0..4 {
            raw.push(r[i].wrapping_sub(prev[i]));
        }
        prev = *r;
    }
    let zx = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    write!(out, "6 0 obj\n").unwrap();
    out.extend_from_slice(&stream(
        "/Type /XRef /Size 7 /W [1 2 1] /Root 1 0 R /Filter /FlateDecode \
         /DecodeParms << /Predictor 12 /Columns 4 >>",
        &zx,
    ));
    write!(out, "\nendobj\nstartxref\n{}\n%%EOF\n", offs[6]).unwrap();

    let f = PdfFile::parse(&out).unwrap();
    let page = f.page(0).unwrap();
    let ents = page.entities(&CurveOptions::default(), true);
    assert_eq!(ents.len(), 1);
    assert_eq!(ents[0].rgb, [255, 0, 0]);
    near(ents[0].width_mm, 2.0 * K, 1e-12);
    let Geom::Line { b, .. } = ents[0].geom else {
        panic!()
    };
    near(b[0], 25.4, 1e-9);
    near(b[1], 50.8, 1e-9);
}

#[test]
fn incremental_update_replaces_content() {
    let base = one_page(b"0 0 m 72 0 l S", "<< >>", "", &[]);
    let prev = {
        let s = String::from_utf8_lossy(&base);
        let p = s.rfind("startxref").unwrap();
        s[p + 9..]
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap()
    };
    let mut out = base.clone();
    let off = out.len();
    out.extend_from_slice(b"4 0 obj\n");
    out.extend_from_slice(&stream("", b"0 0 m 0 144 l S 0 0 m 144 0 l S"));
    out.extend_from_slice(b"\nendobj\n");
    let x = out.len();
    write!(
        out,
        "xref\n4 1\n{off:010} 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R /Prev {prev} >>\nstartxref\n{x}\n%%EOF\n"
    )
    .unwrap();
    let page = PdfFile::parse(&out).unwrap().page(0).unwrap();
    assert_eq!(page.shapes.len(), 2);
}

#[test]
fn broken_xref_is_rebuilt() {
    let mut b = one_page(b"0 0 m 72 0 l S", "<< >>", "", &[]);
    // Shift all offsets: inserting bytes at the start invalidates the table.
    b.splice(9..9, b"% padding padding padding\n".iter().copied());
    let page = PdfFile::parse(&b).unwrap().page(0).unwrap();
    assert_eq!(page.shapes.len(), 1);
    // With no table and no trailer at all.
    let cut = b.windows(4).position(|w| w == b"xref").unwrap();
    let page = PdfFile::parse(&b[..cut]).unwrap().page(0).unwrap();
    assert_eq!(page.shapes.len(), 1);
}

#[test]
fn filters_chain_and_lzw() {
    let content = b"0 0 m 72 72 l S";
    let z = miniz_oxide::deflate::compress_to_vec_zlib(content, 6);
    let hex: String = z.iter().map(|b| format!("{b:02x}")).collect::<String>() + ">";
    let obj = stream("/Filter [/AHx /Fl]", hex.as_bytes());
    let objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents [4 0 R 5 0 R] >>".to_vec(),
        obj,
        // LZW: "0 0 m S" is pre-encoded with 9-bit codes (literals + EOD).
        stream(
            "/Filter /LZWDecode",
            &lzw_encode_literal(b"72 0 m 144 0 l S"),
        ),
    ];
    let page = PdfFile::parse(&pdf(&objs, "")).unwrap().page(0).unwrap();
    assert_eq!(page.shapes.len(), 2);
}

/// LZW without a dictionary: each byte is a literal code; the width grows as in the decoder.
fn lzw_encode_literal(data: &[u8]) -> Vec<u8> {
    let mut codes = vec![256u32];
    codes.extend(data.iter().map(|&b| b as u32));
    codes.push(257);
    let (mut out, mut buf, mut bits) = (vec![], 0u64, 0u32);
    let mut next = 258usize;
    for (i, c) in codes.iter().enumerate() {
        // The decoder adds a dictionary entry after every code except the first after clear.
        let width = if next + 1 >= 512 { 10 } else { 9 };
        buf = buf << width | *c as u64;
        bits += width;
        while bits >= 8 {
            out.push((buf >> (bits - 8)) as u8);
            bits -= 8;
        }
        if i >= 2 {
            next += 1;
        }
    }
    if bits > 0 {
        out.push((buf << (8 - bits)) as u8);
    }
    out
}

#[test]
fn bezier_circle_becomes_arc() {
    // A circle r=72 pt around (200,300) as four cubics, k = 0.5523.
    let (cx, cy, r) = (200.0, 300.0, 72.0);
    let k = 0.5523 * r;
    let c = format!(
        "{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c h S",
        cx + r, cy,
        cx + r, cy + k, cx + k, cy + r, cx, cy + r,
        cx - k, cy + r, cx - r, cy + k, cx - r, cy,
        cx - r, cy - k, cx - k, cy - r, cx, cy - r,
        cx + k, cy - r, cx + r, cy - k, cx + r, cy,
    );
    // A quarter arc (counter-clockwise) and an ellipse (not a circle).
    let quarter = format!(
        " {} {} m {} {} {} {} {} {} c S",
        500.0,
        100.0,
        500.0,
        100.0 + 0.5523 * 50.0,
        450.0 + 0.5523 * 50.0,
        150.0,
        450.0,
        150.0
    );
    let ellipse = " 0 0 m 0 80 20 100 100 100 c S";
    let content = format!("{c}{quarter}{ellipse}");
    let page = PdfFile::parse(&one_page(content.as_bytes(), "<< >>", "", &[]))
        .unwrap()
        .page(0)
        .unwrap();
    let ents = page.entities(&CurveOptions::default(), true);
    assert_eq!(ents.len(), 3);
    let Geom::Arc { c, r: rr, a0, a1 } = ents[0].geom else {
        panic!("{:?}", ents[0].geom)
    };
    near(c[0], cx * K, 1e-3 * r * K);
    near(c[1], cy * K, 1e-3 * r * K);
    near(rr, r * K, 1e-3 * r * K);
    assert_eq!((a0, a1), (0.0, TAU));
    let Geom::Arc { r: qr, a0, a1, .. } = ents[1].geom else {
        panic!("{:?}", ents[1].geom)
    };
    near(qr, 50.0 * K, 0.05 * K);
    near((a0 + PI).rem_euclid(TAU) - PI, 0.0, 1e-3);
    near(a1, PI / 2.0, 1e-3);
    let Geom::Polyline { pts, .. } = &ents[2].geom else {
        panic!("{:?}", ents[2].geom)
    };
    assert!(pts.len() > 3);
}

#[test]
fn rotate_and_cropbox() {
    // A 200×100 pt page, CropBox shifted by (10,20), rotated 90°.
    let page = PdfFile::parse(&one_page(
        b"10 20 m 82 20 l S",
        "<< >>",
        "/MediaBox [0 0 200 100] /CropBox [10 20 210 100] /Rotate 90",
        &[],
    ))
    .unwrap();
    let (w, h) = page.page_size_mm(0).unwrap();
    near(w, 80.0 * K, 1e-9);
    near(h, 190.0 * K, 1e-9);
    let ents = page
        .page(0)
        .unwrap()
        .entities(&CurveOptions::default(), true);
    let Geom::Line { a, b } = ents[0].geom else {
        panic!()
    };
    // (0,0) of the visible area → top-left; the sheet X axis runs down after rotation.
    near(a[0], 0.0, 1e-9);
    near(a[1], 190.0 * K, 1e-9);
    near(b[1], (190.0 - 72.0) * K, 1e-9);
}

#[test]
fn simple_font_text_and_tj() {
    let content = b"BT /F1 12 Tf 1 0 0 1 72 144 Tm [(A) -1000 (B)] TJ (C) Tj ET";
    let font = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 65 /Widths [500 600 700] >>"
        .to_vec();
    let page = PdfFile::parse(&one_page(
        content,
        "<< /Font << /F1 5 0 R >> >>",
        "",
        &[font],
    ))
    .unwrap()
    .page(0)
    .unwrap();
    assert_eq!(page.texts.len(), 2);
    assert_eq!(page.texts[0].string, "AB");
    near(page.texts[0].height_mm, 12.0 * K, 1e-12);
    // A(500) + 1000 + B(600) → 2.1 em × 12 pt.
    assert_eq!(page.texts[1].string, "C");
    near(page.texts[1].pos[0], (72.0 + 2.1 * 12.0) * K, 1e-9);
}

#[test]
fn scale_calibration() {
    near(Scale::Ratio(100.0).factor().unwrap() * 10.0, 1.0, 1e-12);
    let s = Scale::Calibrate {
        a: [10.0, 10.0],
        b: [40.0, 50.0],
        length: 5.0,
    };
    let f = s.factor().unwrap();
    near(f * 50.0, 5.0, 1e-9);
    assert!(Scale::Calibrate {
        a: [1.0, 1.0],
        b: [1.0, 1.0],
        length: 3.0
    }
    .factor()
    .is_err());
    assert!(Scale::Ratio(0.0).factor().is_err());
    assert!(Scale::Factor(f64::NAN).factor().is_err());

    // A 72 pt line = 25.4 mm; calibration "this line = 2.54 m" → ends in metres.
    let page = PdfFile::parse(&one_page(b"0 0 m 72 0 l S", "<< >>", "", &[]))
        .unwrap()
        .page(0)
        .unwrap();
    let opt = ImportOptions {
        scale: Scale::Calibrate {
            a: [0.0, 0.0],
            b: [25.4, 0.0],
            length: 2.54,
        },
        ..Default::default()
    };
    let e = model_entities(&page, &opt).unwrap();
    let Geom::Line { b, .. } = e[0].geom else {
        panic!()
    };
    near(b[0], 2.54, 1e-9);
}

#[test]
fn import_is_one_undo_and_walls_proposed() {
    // Two parallel lines 2 mm apart, 50 mm long: at 1:100 — a 0.2 × 5 m wall.
    let mm = |v: f64| v / K;
    let content = format!(
        "0 0 m {} 0 l S 0 {} m {} {} l S BT /F1 10 Tf 10 10 Td (X) Tj ET",
        mm(50.0),
        mm(2.0),
        mm(50.0),
        mm(2.0)
    );
    let font = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec();
    let page = PdfFile::parse(&one_page(
        content.as_bytes(),
        "<< /Font << /F1 5 0 R >> >>",
        "",
        &[font],
    ))
    .unwrap()
    .page(0)
    .unwrap();
    let opt = ImportOptions {
        scale: Scale::Ratio(100.0),
        ..Default::default()
    };
    let mut m = Model::open(":memory:").unwrap();
    let rep = import_page(&mut m, &page, &opt).unwrap();
    assert_eq!((rep.layers, rep.lines, rep.texts), (1, 2, 1));
    let doc = crate::dxf::export_from(&m).unwrap();
    assert_eq!(doc.entities.len(), 3);
    assert!(doc.layers.iter().any(|l| l.name == "PDF"));
    m.undo().unwrap();
    assert!(crate::dxf::export_from(&m).unwrap().entities.is_empty());

    let walls = propose_walls(&page, &opt, &WallOptions::default()).unwrap();
    assert_eq!(walls.strips.len(), 1, "{:?}", walls.issues);
}

#[test]
fn typed_errors() {
    assert!(matches!(PdfFile::parse(b"hello"), Err(ImportError::NotPdf)));
    let enc = pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec(),
            b"<< /Filter /Standard /V 2 >>".to_vec(),
        ],
        "/Encrypt 3 0 R",
    );
    let e = PdfFile::parse(&enc).err().unwrap();
    assert!(matches!(e, ImportError::Encrypted));
    assert_eq!(e.to_string(), "PDF import: encrypted, not supported");
    let f = PdfFile::parse(&one_page(b"", "<< >>", "", &[])).unwrap();
    assert!(matches!(
        f.page(3),
        Err(ImportError::NoPage { index: 3, count: 1 })
    ));
}

#[test]
fn reference_cycles_terminate() {
    // A page is its own descendant, /Length refers to itself, a form draws itself.
    let objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /XObject << /X 5 0 R >> >> >>"
            .to_vec(),
        b"<< /Length 4 0 R >>\nstream\n0 0 m 1 1 l S /X Do\nendstream".to_vec(),
        stream(
            "/Subtype /Form /Resources << /XObject << /X 5 0 R >> >>",
            b"0 0 m 2 2 l S /X Do",
        ),
    ];
    let page = PdfFile::parse(&pdf(&objs, "")).unwrap().page(0).unwrap();
    assert!(page.truncated);
    assert_eq!(page.shapes.len(), 13);
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn exercise(b: &[u8]) {
    if let Ok(f) = PdfFile::parse(b) {
        for i in 0..f.page_count().min(4) {
            if let Ok(p) = f.page(i) {
                let _ = p.entities(&CurveOptions::default(), true);
            }
        }
    }
}

#[test]
fn malformed_input_never_panics() {
    let mut corpus = vec![export_pdf(&[sheet()]).unwrap()];
    corpus.push(one_page(
        b"q 1 0 0 1 5 5 cm 0 0 m 10 10 20 0 30 30 c h f Q BT /F1 9 Tf (x) Tj ET BI /W 1 ID \x00\xff EI",
        "<< /Font << /F1 5 0 R >> >>",
        "/Rotate 270",
        &[b"<< /Subtype /Type1 >>".to_vec()],
    ));
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for base in &corpus {
        let step = (base.len() / 400).max(1);
        for cut in (0..base.len()).step_by(step) {
            exercise(&base[..cut]);
        }
        for _ in 0..300 {
            let mut b = base.clone();
            for _ in 0..1 + rng.next() % 8 {
                let i = (rng.next() as usize) % b.len();
                b[i] = match rng.next() % 4 {
                    0 => rng.next() as u8,
                    1 => b"0123456789"[(rng.next() % 10) as usize],
                    2 => b"<>[]()/ \n"[(rng.next() % 9) as usize],
                    _ => b[i] ^ 0x80,
                };
            }
            exercise(&b);
        }
    }
}

/// Budget: ≈20 MB of vector content on one page in ≤ 2 s (release).
/// `cargo test --release -p ddc-io pdf_import_budget -- --ignored`
#[test]
#[ignore]
fn pdf_import_budget_20mb() {
    let mut c = Vec::with_capacity(21 << 20);
    let mut i = 0u64;
    while c.len() < 20 << 20 {
        let x = (i % 1000) as f64 * 0.8;
        let y = (i / 1000 % 1000) as f64 * 0.8;
        write!(
            c,
            "{x:.2} {y:.2} m {:.2} {y:.2} l {:.2} {:.2} {:.2} {:.2} {x:.2} {:.2} c S\n",
            x + 0.5,
            x + 0.7,
            y + 0.2,
            x + 0.7,
            y + 0.5,
            y + 0.7
        )
        .unwrap();
        i += 1;
    }
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&c, 1);
    let objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 3370 2384] /Contents 4 0 R >>".to_vec(),
        stream("/Filter /FlateDecode", &z),
    ];
    let file = pdf(&objs, "");
    let t = std::time::Instant::now();
    let f = PdfFile::parse(&file).unwrap();
    let page = f.page(0).unwrap();
    let ents = page.entities(&CurveOptions::default(), true);
    let dt = t.elapsed();
    eprintln!("20 MB page: {} entities in {:?}", ents.len(), dt);
    assert_eq!(page.shapes.len() as u64, i);
    assert!(dt.as_secs_f64() <= 2.0, "{dt:?}");
}

fn flate_zeros(n: usize) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(&vec![b' '; n], 6)
}

#[test]
fn repeated_contents_hit_total_budget() {
    let z = flate_zeros(1 << 20);
    let refs = "5 0 R ".repeat(1000);
    let objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 595 842] >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /Contents [{refs}] /Resources << >> >>").into_bytes(),
        b"null".to_vec(),
        stream("/Filter /FlateDecode", &z),
    ];
    let f = PdfFile::parse(&pdf(&objs, "")).unwrap();
    f.set_decode_budget(10 << 20);
    let t = std::time::Instant::now();
    let r = f.page(0);
    assert!(
        matches!(r, Err(ImportError::Filter(_))),
        "{:?}",
        r.map(|p| p.shapes.len())
    );
    assert!(t.elapsed().as_secs_f64() < 5.0);
    // A single stream within the budget passes.
    let f = PdfFile::parse(&one_page(b"0 0 m 72 0 l S", "<< >>", "", &[])).unwrap();
    f.set_decode_budget(1 << 10);
    assert!(f.page(0).is_ok());
}

#[test]
fn bad_object_stream_decoded_once() {
    let objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 595 842] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /Resources << >> >>".to_vec(),
        stream(
            "/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode",
            b"garbage",
        ),
    ];
    let mut f = PdfFile::parse(&pdf(&objs, "")).unwrap();
    f.doc.test_add_instream(1000, 100_000, 4);
    let before = f.doc.decode_attempts.get();
    for n in 1000..101_000u32 {
        let _ = f.doc.resolve(&Obj::Ref(n, 0));
    }
    assert_eq!(f.doc.decode_attempts.get() - before, 1);
}

#[test]
fn flat_cubic_stays_bezier_real_arc_converts() {
    let f = PdfFile::parse(&one_page(b"0 0 m 33 3.1 67 -3 100 0 c S", "<< >>", "", &[])).unwrap();
    let page = f.page(0).unwrap();
    let ents = page.entities(&CurveOptions::default(), true);
    assert!(ents.iter().all(|e| !matches!(e.geom, Geom::Arc { .. })));
    bezier_circle_becomes_arc();
}

#[test]
fn bdc_overflow_ignores_matching_emc() {
    let mut c = String::from("/OC /P0 BDC ");
    // Walls is the 4096th on the stack; 4 more BMCs overflow it.
    c = format!("{}", "/Span BMC ".repeat(4095)) + &c;
    c += &"/Span BMC ".repeat(4);
    c += &"EMC ".repeat(4);
    c += "0 0 m 72 0 l S";
    let objs = vec![b"<< /Type /OCG /Name (Walls) >>".to_vec()];
    let f = PdfFile::parse(&one_page(
        c.as_bytes(),
        "<< /Properties << /P0 5 0 R >> >>",
        "",
        &objs,
    ))
    .unwrap();
    let page = f.page(0).unwrap();
    let ents = page.entities(&CurveOptions::default(), true);
    assert_eq!(page.layers[ents[0].layer], "Walls");
}
