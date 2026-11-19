// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::facts::{from_extracted, norms_in, rooms_of_table, FactKind};
use super::*;
use crate::ru_fixtures::ru;
use std::io::Write;

/// One A4 page with Helvetica as /F1; `content` is the page stream.
pub fn pdf_page(content: &str) -> Vec<u8> {
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 595 842] >>".into(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = vec![];
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        write!(out, "{} 0 obj\n{o}\nendobj\n", i + 1).unwrap();
    }
    let x = out.len();
    write!(out, "xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).unwrap();
    for o in offs {
        write!(out, "{o:010} 00000 n \n").unwrap();
    }
    write!(out, "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).unwrap();
    out
}

fn text(x: f64, y: f64, s: &str) -> String {
    format!("BT /F1 10 Tf {x} {y} Td ({s}) Tj ET\n")
}

/// A brief with two dimensioned 60 mm lines (6000 written over each: 1:100).
fn brief_pdf(scale_text: bool) -> Vec<u8> {
    let mm = 72.0 / 25.4;
    let len = 60.0 * mm;
    let mut c = String::new();
    for y in [300.0, 400.0] {
        c.push_str(&format!("100 {y} m {} {y} l S\n", 100.0 + len));
        c.push_str(&text(100.0 + len / 2.0 - 10.0, y + 3.0, "6000"));
    }
    c.push_str(&text(60.0, 780.0, "Site 30 x 40 m"));
    c.push_str(&text(60.0, 760.0, "Storeys: 3"));
    c.push_str(&text(60.0, 740.0, "Kitchen - 12.5 m2"));
    c.push_str(&text(60.0, 720.0, "Bedroom: 14 m2"));
    c.push_str(&text(60.0, 700.0, "Design to EN 1991-1-1 and ISO 19650"));
    c.push_str(&text(60.0, 680.0, "Building height no more than 12 m"));
    if scale_text {
        c.push_str(&text(400.0, 60.0, "Scale 1:50"));
    }
    pdf_page(&c)
}

#[test]
fn pdf_gives_text_lines_and_a_scale_from_dimensions() {
    let bytes = brief_pdf(false);
    assert_eq!(detect("brief.pdf", &bytes), Some(Kind::Pdf));
    let e = extract(Kind::Pdf, &bytes).unwrap();
    assert_eq!(e.page_count, 1);
    assert_eq!(e.lines, 2);
    assert!(e.pages[0].contains("Site 30 x 40 m"), "{}", e.pages[0]);
    let s = e.scale.clone().expect("scale from two dimensions");
    assert_eq!(s.ratio, 100.0);
    assert_eq!(s.src, ScaleSrc::Dimensions(2));

    let facts = from_extracted("brief.pdf", Kind::Pdf, &e);
    let one = |k: FactKind| facts.iter().find(|f| f.kind == k).unwrap_or_else(|| panic!("{k:?} in {facts:?}"));
    assert_eq!(one(FactKind::Site).value, "30 \u{d7} 40 m");
    assert_eq!(one(FactKind::Storeys).count, Some(3));
    assert_eq!(one(FactKind::Scale).value, "1:100");
    assert!(one(FactKind::Height).value.starts_with("12 m"));
    let rooms: Vec<_> = facts.iter().filter(|f| f.kind == FactKind::Room).collect();
    assert_eq!(rooms.len(), 2, "{rooms:?}");
    assert_eq!((rooms[0].label.as_str(), rooms[0].area_m2), ("Kitchen", Some(12.5)));
    assert_eq!(rooms[0].src.page, Some(1));
    assert_eq!(rooms[0].src.region, "line 3");
    assert_eq!(rooms[0].src.show(), "brief.pdf, p. 1, line 3");
    let norms: Vec<_> = facts.iter().filter(|f| f.kind == FactKind::Norm).map(|f| f.value.as_str()).collect();
    assert_eq!(norms, ["EN 1991-1-1", "ISO 19650"]);
}

#[test]
fn a_written_scale_wins_over_dimensions() {
    let e = extract(Kind::Pdf, &brief_pdf(true)).unwrap();
    let s = e.scale.unwrap();
    assert_eq!(s.ratio, 50.0);
    assert!(matches!(s.src, ScaleSrc::Text(ref t) if t.contains("1:50")));
}

#[test]
fn one_dimension_is_no_scale() {
    let mm = 72.0 / 25.4;
    let c = format!("100 300 m {} 300 l S\n{}", 100.0 + 60.0 * mm, text(170.0, 303.0, "6000"));
    assert_eq!(extract(Kind::Pdf, &pdf_page(&c)).unwrap().scale, None);
}

#[test]
fn scale_text_needs_a_standard_ratio() {
    assert_eq!(scale_in_text("M 1:100").map(|s| s.0), Some(100.0));
    assert_eq!(scale_in_text(ru("scale_word")).map(|s| s.0), Some(200.0));
    assert_eq!(scale_in_text("ratio 1:37"), None);
    assert_eq!(scale_in_text("time 11:30"), None);
    assert_eq!(scale_in_text("v1:1"), None);
}

fn programme_xlsx() -> Vec<u8> {
    let mut wb = rust_xlsxwriter::Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name(ru("programme")).unwrap();
    ws.write(0, 0, ru("programme_rooms")).unwrap();
    for (c, h) in [ru("hdr_room"), ru("hdr_area_m2"), ru("hdr_qty")].iter().enumerate() {
        ws.write(2, c as u16, *h).unwrap();
    }
    let rows = [(ru("living_room"), 24.0, 1.0), (ru("bedroom"), 14.5, 2.0), (ru("bathroom"), 4.2, 2.0)];
    for (i, (n, a, k)) in rows.iter().enumerate() {
        let r = 3 + i as u32;
        ws.write(r, 0, *n).unwrap();
        ws.write(r, 1, *a).unwrap();
        ws.write(r, 2, *k).unwrap();
    }
    ws.write(6, 0, ru("total")).unwrap();
    ws.write(6, 1, 61.4).unwrap();
    wb.save_to_buffer().unwrap()
}

#[test]
fn xlsx_programme_gives_rooms_with_rows() {
    let bytes = programme_xlsx();
    assert_eq!(detect("prog.xlsx", &bytes), Some(Kind::Sheet));
    let e = extract(Kind::Sheet, &bytes).unwrap();
    assert!(e.pages[0].starts_with(&format!("[{}]", ru("programme"))));
    let facts = from_extracted("prog.xlsx", Kind::Sheet, &e);
    let rooms: Vec<_> = facts.iter().filter(|f| f.kind == FactKind::Room).collect();
    assert_eq!(rooms.len(), 3, "the total row is no room: {rooms:?}");
    assert_eq!(rooms[1].label, ru("bedroom"));
    assert_eq!(rooms[1].area_m2, Some(14.5));
    assert_eq!(rooms[1].count, Some(2));
    assert_eq!(rooms[1].src.region, "row 5");
    assert_eq!(rooms[1].src.page, None);
}

#[test]
fn csv_with_semicolons_and_comma_decimals() {
    let text = "Room;Area;Qty\nOffice;18,5;4\n\"Meeting, large\";32;1\n";
    let e = extract(Kind::Csv, text.as_bytes()).unwrap();
    assert_eq!(e.table[2][0], "Meeting, large");
    let rooms = rooms_of_table("p.csv", &e.table);
    assert_eq!(rooms.len(), 2);
    assert_eq!((rooms[0].area_m2, rooms[0].count), (Some(18.5), Some(4)));
}

#[test]
fn dxf_plan_is_read_as_a_drawing() {
    let text = crate::drawing_model::fixture::sample_plan_text(0);
    assert_eq!(detect("plan.dxf", text.as_bytes()), Some(Kind::Dxf));
    let e = extract(Kind::Dxf, text.as_bytes()).unwrap();
    assert!(e.lines >= 4, "{}", e.lines);
    assert!(Kind::Dxf.is_drawing());
}

#[test]
fn russian_brief_lines() {
    let e = one_page(ru("brief_text").into());
    let f = from_extracted(ru("brief_file"), Kind::Text, &e);
    let get = |k| f.iter().filter(|x| x.kind == k).collect::<Vec<_>>();
    assert_eq!(get(FactKind::Site)[0].value, "30 \u{d7} 40 m");
    assert_eq!(get(FactKind::Storeys)[0].count, Some(5));
    assert_eq!(get(FactKind::Room)[0].label, ru("kitchen"));
    assert_eq!(get(FactKind::Room)[0].area_m2, Some(12.5));
    assert_eq!(get(FactKind::TotalArea)[0].area_m2, Some(1200.0));
    assert!(get(FactKind::Height)[0].value.starts_with("18 m"));
    assert_eq!(get(FactKind::Constraint).len(), 1);
    assert_eq!(get(FactKind::Room)[0].src.page, None, "a text file has no pages");
    assert_eq!(norms_in(ru("norms_sentence")), [ru("norm_sp"), ru("norm_gost")]);
}

#[test]
fn docx_paragraph_text() {
    let xml = r#"<w:document><w:body><w:p><w:r><w:t>Brief</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Hall </w:t></w:r><w:r><w:t>40 m2 &amp; more</w:t></w:r></w:p></w:body></w:document>"#;
    let mut buf = Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        z.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(xml.as_bytes()).unwrap();
        z.finish().unwrap();
    }
    let bytes = buf.into_inner();
    assert_eq!(detect("b.docx", &bytes), Some(Kind::Docx));
    let e = extract(Kind::Docx, &bytes).unwrap();
    assert_eq!(e.pages[0], "Brief\nHall 40 m2 & more");
}

#[test]
fn ifc_storeys_and_spaces() {
    let t = "ISO-10303-21;\nDATA;\n#1=IFCBUILDINGSTOREY('a',$,'Level 1',$,$,$,$,$,.ELEMENT.,0.);\n#2=IFCBUILDINGSTOREY('b',$,'Level 2',$,$,$,$,$,.ELEMENT.,3.);\n#3=IFCSPACE('c',$,'101',$);\nENDSEC;";
    assert_eq!(detect("m.ifc", t.as_bytes()), Some(Kind::Ifc));
    let e = extract(Kind::Ifc, t.as_bytes()).unwrap();
    assert_eq!(e.storeys, ["Level 1", "Level 2"]);
    assert_eq!(e.spaces, 1);
    let f = from_extracted("m.ifc", Kind::Ifc, &e);
    assert_eq!(f.iter().find(|x| x.kind == FactKind::Storeys).unwrap().count, Some(2));
}

#[test]
fn unknown_and_oversized_are_refused() {
    assert_eq!(detect("a.exe", b"MZ"), None);
    assert_eq!(detect("a.xlsx", b"not a zip"), None);
    let big = vec![b' '; MAX_BYTES as usize + 1];
    assert!(extract(Kind::Text, &big).is_err());
}

#[test]
fn numbers_keep_whole_tens() {
    assert_eq!(super::facts::fmt(100.0), "100");
    assert_eq!(super::facts::fmt(12.5), "12.5");
    assert_eq!(super::facts::fmt(1200.0), "1200");
}

#[test]
fn text_decodes_bom_and_single_byte() {
    assert_eq!(decode_text(b"\xEF\xBB\xBFabc"), "abc");
    assert_eq!(decode_text(&[0xCA, 0xF3, 0xF5, 0xED, 0xFF]), ru("kitchen"));
}
