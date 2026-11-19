// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use std::time::Instant;

/// The PDF + the decompressed contents of all Flate streams, for substring search.
fn inflated(pdf: &[u8]) -> Vec<u8> {
    let mut all = pdf.to_vec();
    for s in streams(pdf) {
        all.extend_from_slice(&s);
    }
    all
}

fn streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    let mut res = Vec::new();
    let mut i = 0;
    while let Some(p) = find(&pdf[i..], b">>\nstream\n") {
        let start = i + p + 10;
        let end = start + find(&pdf[start..], b"\nendstream").unwrap();
        if let Ok(d) = miniz_oxide::inflate::decompress_to_vec_zlib(&pdf[start..end]) {
            res.push(d);
        }
        i = end;
    }
    res
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn has(hay: &[u8], needle: &str) -> bool {
    let all = inflated(hay);
    find(&all, needle.as_bytes()).is_some()
}

fn a1(n: usize) -> Sheet {
    let mut items = Vec::with_capacity(n + 1);
    for i in 0..n {
        let x = (i % 100) as f64 * 8.0;
        let y = (i / 100) as f64 * 5.0;
        items.push(Item::Polyline {
            pts: vec![[x, y], [x + 7.5, y + 3.25]],
            closed: false,
            weight_mm: 0.25,
            rgb: [200, 30, 30],
            layer: i % 2,
        });
    }
    items.push(Item::Text {
        pos: [100.0, 100.0],
        height_mm: 5.0,
        rotation: 90.0,
        string: "Wall (A-101)".into(),
        rgb: [0, 0, 0],
        layer: 1,
    });
    Sheet {
        width_mm: 841.0,
        height_mm: 594.0,
        layers: vec![
            SheetLayer {
                name: "A-WALL".into(),
                visible: true,
            },
            SheetLayer {
                name: "A-ANNO".into(),
                visible: false,
            },
        ],
        items,
    }
}

#[test]
fn a1_10k_lines() {
    let sh = a1(10_000);
    let t = Instant::now();
    let pdf = export_pdf(&[sh]).unwrap();
    let dt = t.elapsed();
    println!("pdf export 10k lines: {:?}, {} bytes", dt, pdf.len());
    assert!(dt.as_millis() < 2000);
    assert!(pdf.starts_with(b"%PDF"));
    assert!(has(&pdf, "/OCProperties"));
    // "A-WALL" and "A-ANNO" as UTF-16BE hex
    assert!(has(&pdf, "<FEFF0041002D00570041004C004C>"));
    assert!(has(&pdf, "<FEFF0041002D0041004E004E004F>"));
    assert!(has(&pdf, "/OFF [5 0 R ]"));
    let plain = export_pdf_with_font(&[a1(3)], None).unwrap();
    assert!(has(&plain, "(Wall \\(A-101\\)) Tj"));
    assert!(has(&pdf, "/Count 1"));
}

#[test]
fn two_sheets_two_pages() {
    let pdf = export_pdf(&[a1(3), a1(5)]).unwrap();
    assert!(has(&pdf, "/Count 2"));
    let pages = pdf.windows(15).filter(|w| *w == b"/Type /Page /Pa").count();
    assert_eq!(pages, 2);
}

fn text_sheet(s: &str) -> Sheet {
    Sheet {
        width_mm: 297.0,
        height_mm: 210.0,
        layers: vec![],
        items: vec![Item::Text {
            pos: [10.0, 10.0],
            height_mm: 3.5,
            rotation: 0.0,
            string: s.into(),
            rgb: [255, 0, 0],
            layer: 0,
        }],
    }
}

fn hex16(s: &str) -> Vec<u16> {
    (0..s.len() / 4)
        .map(|i| u16::from_str_radix(&s[4 * i..4 * i + 4], 16).unwrap())
        .collect()
}

#[test]
fn unicode_text_roundtrips_via_tounicode() {
    let text = format!("{} A-101 — 3000 mm", crate::ru_fixtures::ru("wall_word"));
    let pdf = export_pdf(&[text_sheet(&text)]).unwrap();
    let st = streams(&pdf);
    let utf = |d: &Vec<u8>| String::from_utf8_lossy(d).into_owned();
    let cmap = st
        .iter()
        .map(utf)
        .find(|s| s.contains("beginbfchar"))
        .unwrap();
    let content = st.iter().map(utf).find(|s| s.contains(" Tj")).unwrap();
    assert!(content.contains("1 0 0 rg"));

    let mut map = std::collections::HashMap::new();
    for l in cmap.lines() {
        if let Some((a, b)) = l.strip_prefix('<').and_then(|l| l.split_once("> <")) {
            let b = b.trim_end_matches('>');
            map.insert(hex16(a)[0], String::from_utf16(&hex16(b)).unwrap());
        }
    }
    let hexs = content
        .split_once("Tm <")
        .unwrap()
        .1
        .split_once("> Tj")
        .unwrap()
        .0;
    let got: String = hex16(hexs).iter().map(|c| map[c].as_str()).collect();
    assert_eq!(got, text);
    assert!(has(&pdf, "/CIDFontType2"));
    assert!(has(&pdf, "/FontFile2"));
    // a subset, not the whole font (~620 KB)
    assert!(pdf.len() < 60_000, "{}", pdf.len());
}

#[test]
fn compression_shrinks_10k_sheet_3x() {
    let sh = a1(10_000);
    let packed = export_pdf(&[sh.clone()]).unwrap().len();
    let raw = writer::content(&sh, None).len();
    println!("content raw {raw}, pdf {packed}");
    assert!(raw >= 3 * packed, "raw {raw} packed {packed}");
}

#[test]
fn non_finite_input_is_rejected() {
    let mut sh = text_sheet("x");
    sh.width_mm = f64::NAN;
    assert!(export_pdf(&[sh]).is_err());
    let mut sh = a1(3);
    if let Item::Polyline { pts, .. } = &mut sh.items[0] {
        pts[0][0] = f64::INFINITY;
    }
    assert!(export_pdf(&[sh]).is_err());
    let mut sh = text_sheet("x");
    if let Item::Text { rotation, .. } = &mut sh.items[0] {
        *rotation = f64::INFINITY;
    }
    assert!(export_pdf_with_font(&[sh], None).is_err());
}

#[test]
fn cjk_text_uses_fallback_font_and_extracts_back() {
    let fonts = cjk_fallback_fonts();
    let Some(fb) = (fonts.len() == 2).then_some(&fonts) else {
        eprintln!("SKIP: CJK font not found; run tools/fetch_cjk_font.ps1 or set DDC_CJK_FONT_DIR");
        return;
    };
    let text = "平面图 1F 図面 도면";
    let pdf =
        export_pdf_with_fonts(&[text_sheet(text)], Some(EMBEDDED_FONT), &[&fb[0], &fb[1]]).unwrap();
    assert!(pdf.len() < 200_000, "{}", pdf.len());
    let st = streams(&pdf);
    let utf = |d: &Vec<u8>| String::from_utf8_lossy(d).into_owned();
    let content = st.iter().map(utf).find(|s| s.contains(" Tj")).unwrap();
    // CMap of each font: cid -> text; Tj runs: (font, hex).
    let cmaps: Vec<_> = st
        .iter()
        .map(utf)
        .filter(|s| s.contains("beginbfchar"))
        .collect();
    assert_eq!(cmaps.len(), 3);
    let parse = |cmap: &str| {
        let mut map = std::collections::HashMap::new();
        for l in cmap.lines() {
            if let Some((a, b)) = l.strip_prefix('<').and_then(|l| l.split_once("> <")) {
                let b = b.trim_end_matches('>');
                map.insert(hex16(a)[0], String::from_utf16(&hex16(b)).unwrap());
            }
        }
        map
    };
    let maps: Vec<_> = cmaps.iter().map(|c| parse(c)).collect();
    let mut font = 0;
    let mut got = String::new();
    for tok in content.split(&[' ', '\n'][..]) {
        if tok == "/F1" {
            font = 0;
        } else if tok == "/F2" {
            font = 1;
        } else if tok == "/F3" {
            font = 2;
        } else if let Some(h) = tok.strip_prefix('<').and_then(|t| t.strip_suffix('>')) {
            got.extend(hex16(h).iter().map(|c| maps[font][c].as_str()));
        }
    }
    assert_eq!(got, text);
}

fn logo_sheet(alpha: u8, copies: usize) -> Sheet {
    let px: Vec<u8> = (0..4).flat_map(|i| [i * 40, 10, 200, alpha]).collect();
    let image = std::sync::Arc::new(PdfImage {
        width: 2,
        height: 2,
        rgba: px,
    });
    Sheet {
        width_mm: 100.0,
        height_mm: 50.0,
        layers: vec![SheetLayer {
            name: "Logo".into(),
            visible: true,
        }],
        items: (0..copies)
            .map(|k| Item::Image {
                rect: [5.0 + k as f64 * 20.0, 5.0, 16.0, 8.0],
                image: image.clone(),
                layer: 0,
            })
            .collect(),
    }
}

#[test]
fn image_is_an_xobject_and_transparency_becomes_a_soft_mask() {
    let pdf = export_pdf(&[logo_sheet(128, 1)]).unwrap();
    assert!(find(&pdf, b"/Subtype /Image").is_some());
    assert!(find(&pdf, b"/SMask").is_some(), "alpha kept as a soft mask");
    assert!(find(&pdf, b"/XObject << /Im0").is_some());
    assert!(has(&pdf, "/Im0 Do"), "the page draws the image");
    let opaque = export_pdf(&[logo_sheet(255, 1)]).unwrap();
    assert!(find(&opaque, b"/Subtype /Image").is_some());
    assert!(find(&opaque, b"/SMask").is_none(), "opaque image needs no mask");
}

#[test]
fn one_image_used_twice_is_stored_once() {
    let pdf = export_pdf(&[logo_sheet(255, 3), logo_sheet(255, 1)]).unwrap();
    let n = pdf.windows(15).filter(|w| *w == b"/Subtype /Image").count();
    assert_eq!(n, 1);
    let mut book = PdfBook::new(Vec::new()).unwrap();
    book.add_page(&logo_sheet(128, 2)).unwrap();
    book.add_page(&logo_sheet(128, 2)).unwrap();
    let bytes = book.finish().unwrap();
    // one image and its mask, shared by both pages
    assert_eq!(bytes.windows(15).filter(|w| *w == b"/Subtype /Image").count(), 2);
    assert!(find(&bytes, b"/XObject << /Im0").is_some());
}

#[test]
fn malformed_image_is_refused() {
    let mut sh = logo_sheet(255, 1);
    if let Item::Image { image, .. } = &mut sh.items[0] {
        *image = std::sync::Arc::new(PdfImage {
            width: 3,
            height: 3,
            rgba: vec![0; 8],
        });
    }
    assert!(export_pdf(&[sh]).is_err());
}
