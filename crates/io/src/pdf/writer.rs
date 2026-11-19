// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! PDF 1.5 assembly: catalog, pages per sheet, a shared OCG set, Helvetica/WinAnsi.
//! Text: an embedded TrueType subset (Type0/Identity-H, ToUnicode); without a font —
//! Helvetica/WinAnsi (other characters are replaced with "?"). Streams are FlateDecode-compressed.

use super::font::FontSubset;
use super::{Item, PdfImage, Sheet};
use std::sync::Arc;
use std::io::Write;

pub(super) const PT_PER_MM: f64 = 72.0 / 25.4;

fn winansi(c: char) -> Option<u8> {
    let u = c as u32;
    if (0x20..0x7f).contains(&u) || (0xa0..=0xff).contains(&u) {
        return Some(u as u8);
    }
    Some(match c {
        '€' => 0x80,
        '‚' => 0x82,
        '„' => 0x84,
        '…' => 0x85,
        '‘' => 0x91,
        '’' => 0x92,
        '“' => 0x93,
        '”' => 0x94,
        '•' => 0x95,
        '–' => 0x96,
        '—' => 0x97,
        _ => return None,
    })
}

fn write_text_string(out: &mut Vec<u8>, s: &str) {
    out.push(b'(');
    for c in s.chars() {
        let b = winansi(c).unwrap_or(b'?');
        match b {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(b);
            }
            0x20..=0x7e => out.push(b),
            _ => write!(out, "\\{:03o}", b).unwrap(),
        }
    }
    out.push(b')');
}

/// Layer name as UTF-16BE (hex) so names are not lost.
pub(super) fn write_utf16_hex(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(b"<FEFF");
    for u in s.encode_utf16() {
        write!(out, "{:04X}", u).unwrap();
    }
    out.push(b'>');
}

/// All sheet numbers must be finite: otherwise `NaN`/`inf` would appear in the stream.
pub(super) fn validate(sheets: &[Sheet]) -> Result<(), super::PdfError> {
    let bad =
        |what: &str, i: usize| Err(super::PdfError(format!("non-finite {what} in sheet {i}")));
    for (i, sh) in sheets.iter().enumerate() {
        if !(sh.width_mm.is_finite() && sh.height_mm.is_finite()) {
            return bad("sheet size", i);
        }
        for it in &sh.items {
            let ok = match it {
                Item::Polyline { pts, weight_mm, .. } => {
                    weight_mm.is_finite()
                        && pts.iter().all(|p| p[0].is_finite() && p[1].is_finite())
                }
                Item::Text {
                    pos,
                    height_mm,
                    rotation,
                    ..
                } => {
                    pos.iter().all(|v| v.is_finite())
                        && height_mm.is_finite()
                        && rotation.is_finite()
                }
                Item::Fill { rings, .. } => rings
                    .iter()
                    .flatten()
                    .all(|p| p[0].is_finite() && p[1].is_finite()),
                Item::Image { rect, image, .. } => {
                    rect.iter().all(|v| v.is_finite())
                        && image.width > 0
                        && image.height > 0
                        && image.rgba.len() as u64
                            == u64::from(image.width) * u64::from(image.height) * 4
                }
            };
            if !ok {
                return bad("coordinate", i);
            }
        }
    }
    Ok(())
}

pub(super) fn num(out: &mut Vec<u8>, v: f64) {
    debug_assert!(v.is_finite());
    // 3 decimals (micrometres), no trailing zeros.
    let mut s = format!("{:.3}", v);
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        s = "0".into();
    }
    out.extend_from_slice(s.as_bytes());
    out.push(b' ');
}

fn item_layer(it: &Item) -> usize {
    match it {
        Item::Polyline { layer, .. }
        | Item::Text { layer, .. }
        | Item::Fill { layer, .. }
        | Item::Image { layer, .. } => *layer,
    }
}

fn zlib(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

pub(super) fn write_stream(out: &mut Vec<u8>, extra: &str, data: &[u8]) {
    let z = zlib(data);
    writeln!(
        out,
        "<< {}/Filter /FlateDecode /Length {} >>\nstream",
        extra,
        z.len()
    )
    .unwrap();
    out.extend_from_slice(&z);
    out.extend_from_slice(b"\nendstream");
}

pub(super) fn content(sheet: &Sheet, font: Option<&Fonts>) -> Vec<u8> {
    let mut o = Vec::with_capacity(sheet.items.len() * 48 + 64);
    // From here on all coordinates are in mm.
    write!(o, "{0:.6} 0 0 {0:.6} 0 0 cm\n1 J 1 j\n", PT_PER_MM).unwrap();
    let n = sheet.layers.len();
    let images = sheet_images(sheet);
    // One BDC/EMC block per layer; items with an invalid layer index are drawn outside groups.
    for li in 0..=n {
        let mut opened = false;
        for it in &sheet.items {
            let l = item_layer(it).min(n);
            if l != li {
                continue;
            }
            if li < n && !opened {
                writeln!(o, "/OC /L{} BDC", li).unwrap();
                opened = true;
            }
            match it {
                Item::Polyline { .. } => emit_poly(&mut o, it),
                Item::Text { .. } => emit_text(&mut o, it, font),
                Item::Fill { .. } => emit_fill(&mut o, it),
                Item::Image { .. } => emit_image(&mut o, it, &images),
            }
        }
        if opened {
            o.extend_from_slice(b"EMC\n");
        }
    }
    o
}

/// Distinct sheet images in order of appearance: the index is the `/Im<k>` name in the page resources.
pub(super) fn sheet_images(sh: &Sheet) -> Vec<Arc<PdfImage>> {
    let mut v: Vec<Arc<PdfImage>> = Vec::new();
    for it in &sh.items {
        if let Item::Image { image, .. } = it {
            if !v.iter().any(|x| Arc::ptr_eq(x, image) || **x == **image) {
                v.push(image.clone());
            }
        }
    }
    v
}

fn emit_image(o: &mut Vec<u8>, it: &Item, images: &[Arc<PdfImage>]) {
    let Item::Image { rect, image, .. } = it else {
        return;
    };
    let Some(k) = images
        .iter()
        .position(|x| Arc::ptr_eq(x, image) || **x == **image)
    else {
        return;
    };
    o.extend_from_slice(b"q ");
    for v in [rect[2], 0.0, 0.0, rect[3], rect[0], rect[1]] {
        num(o, v);
    }
    o.extend_from_slice(format!("cm /Im{k} Do Q
").as_bytes());
}

/// Image objects: a transparency mask (if any pixels are not fully opaque) and the
/// DeviceRGB image itself; returns the image object number.
pub(super) fn write_image(out: &mut Vec<u8>, offs: &mut Vec<usize>, img: &PdfImage) -> usize {
    let n = img.width as usize * img.height as usize;
    let mut rgb = Vec::with_capacity(n * 3);
    let mut alpha = Vec::with_capacity(n);
    for p in img.rgba.chunks_exact(4) {
        rgb.extend_from_slice(&p[..3]);
        alpha.push(p[3]);
    }
    let head = format!(
        "/Type /XObject /Subtype /Image /Width {} /Height {} /BitsPerComponent 8 ",
        img.width, img.height
    );
    let smask = if alpha.iter().any(|&a| a != 255) {
        begin(out, offs);
        write_stream(out, &format!("{head}/ColorSpace /DeviceGray "), &alpha);
        end(out);
        Some(offs.len())
    } else {
        None
    };
    begin(out, offs);
    let sm = smask.map_or(String::new(), |s| format!("/SMask {s} 0 R "));
    write_stream(out, &format!("{head}/ColorSpace /DeviceRGB {sm}"), &rgb);
    end(out);
    offs.len()
}

/// The number of objects `write_image` will occupy.
pub(super) fn image_objects(img: &PdfImage) -> usize {
    1 + usize::from(img.rgba.chunks_exact(4).any(|p| p[3] != 255))
}

fn emit_poly(o: &mut Vec<u8>, it: &Item) {
    let Item::Polyline {
        pts,
        closed,
        weight_mm,
        rgb,
        ..
    } = it
    else {
        return;
    };
    if pts.len() < 2 {
        return;
    }
    for c in rgb {
        num(o, *c as f64 / 255.0);
    }
    o.extend_from_slice(b"RG ");
    num(o, weight_mm.max(0.0));
    o.extend_from_slice(b"w ");
    num(o, pts[0][0]);
    num(o, pts[0][1]);
    o.extend_from_slice(b"m ");
    for p in &pts[1..] {
        num(o, p[0]);
        num(o, p[1]);
        o.extend_from_slice(b"l ");
    }
    o.extend_from_slice(if *closed { b"s\n" } else { b"S\n" });
}

fn emit_fill(o: &mut Vec<u8>, it: &Item) {
    let Item::Fill { rings, rgb, .. } = it else {
        return;
    };
    let rings: Vec<&Vec<[f64; 2]>> = rings.iter().filter(|r| r.len() >= 3).collect();
    if rings.is_empty() {
        return;
    }
    for c in rgb {
        num(o, *c as f64 / 255.0);
    }
    o.extend_from_slice(b"rg ");
    for r in rings {
        num(o, r[0][0]);
        num(o, r[0][1]);
        o.extend_from_slice(b"m ");
        for p in &r[1..] {
            num(o, p[0]);
            num(o, p[1]);
            o.extend_from_slice(b"l ");
        }
        o.extend_from_slice(b"h ");
    }
    o.extend_from_slice(b"f*\n");
}

fn emit_text(o: &mut Vec<u8>, it: &Item, font: Option<&Fonts>) {
    let Item::Text {
        pos,
        height_mm,
        rotation,
        string,
        rgb,
        ..
    } = it
    else {
        return;
    };
    let (s, c) = rotation.to_radians().sin_cos();
    o.extend_from_slice(b"BT ");
    for c in rgb {
        num(o, *c as f64 / 255.0);
    }
    o.extend_from_slice(b"rg /F1 ");
    num(o, *height_mm);
    o.extend_from_slice(b"Tf ");
    for v in [c, s, -s, c, pos[0], pos[1]] {
        num(o, v);
    }
    o.extend_from_slice(b"Tm ");
    match font {
        Some(f) => {
            // Runs per font: F1 primary, F2 fallback (CJK).
            let mut runs: Vec<(usize, String)> = Vec::new();
            for ch in string.chars() {
                let k = f.pick(ch);
                match runs.last_mut() {
                    Some((rk, r)) if *rk == k => r.push(ch),
                    _ => runs.push((k, ch.to_string())),
                }
            }
            if runs.is_empty() {
                runs.push((0, String::new()));
            }
            for (i, (k, r)) in runs.iter().enumerate() {
                if i > 0 || *k > 0 {
                    o.extend_from_slice(format!("/F{} ", k + 1).as_bytes());
                    num(o, *height_mm);
                    o.extend_from_slice(b"Tf ");
                }
                let fs = if *k == 0 {
                    &f.primary
                } else {
                    &f.fallbacks[*k - 1]
                };
                o.extend_from_slice(fs.encode(r).as_bytes());
                o.extend_from_slice(if i + 1 == runs.len() {
                    b" Tj ET\n"
                } else {
                    b" Tj "
                });
            }
        }
        None => {
            write_text_string(o, string);
            o.extend_from_slice(b" Tj ET\n");
        }
    }
}

/// The primary font and an optional fallback (CJK) — subsets by the characters actually needed.
pub(super) struct Fonts {
    pub primary: FontSubset,
    pub fallbacks: Vec<FontSubset>,
}

impl Fonts {
    /// 0 — the primary font, k>0 — the k-th fallback: the first where the character has a glyph.
    fn pick(&self, c: char) -> usize {
        let has = |f: &FontSubset| f.char_cid.get(&c).is_some_and(|&g| g != 0);
        if has(&self.primary) {
            return 0;
        }
        self.fallbacks.iter().position(has).map_or(0, |k| k + 1)
    }
}

pub(super) fn used_chars(sheets: &[Sheet]) -> std::collections::BTreeSet<char> {
    sheets
        .iter()
        .flat_map(|s| &s.items)
        .filter_map(|it| match it {
            Item::Text { string, .. } => Some(string.chars()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// CJK fallback fonts (SC for han/kana, KR for hangul): files in `DDC_CJK_FONT_DIR`, otherwise
/// `assets/fonts/cjk/` (placed by `tools/fetch_cjk_font.ps1`; not kept in git because of their size).
pub fn cjk_fallback_fonts() -> Vec<Vec<u8>> {
    let dir = std::env::var_os("DDC_CJK_FONT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/cjk")
        });
    ["NotoSansSC.ttf", "NotoSansKR.ttf"]
        .iter()
        .filter_map(|n| std::fs::read(dir.join(n)).ok())
        .collect()
}

/// Export with the embedded Noto Sans font (OFL); CJK characters come from the fallback font
/// if available.
pub fn export_pdf(sheets: &[Sheet]) -> Result<Vec<u8>, super::PdfError> {
    let need = ttf_parser::Face::parse(EMBEDDED_FONT, 0)
        .ok()
        .is_some_and(|face| {
            used_chars(sheets)
                .iter()
                .any(|&c| face.glyph_index(c).is_none() && !c.is_whitespace() && !c.is_control())
        });
    let fb = if need {
        cjk_fallback_fonts()
    } else {
        Vec::new()
    };
    let refs: Vec<&[u8]> = fb.iter().map(|v| v.as_slice()).collect();
    export_pdf_with_fonts(sheets, Some(EMBEDDED_FONT), &refs)
}

pub const EMBEDDED_FONT: &[u8] = include_bytes!("../../assets/fonts/NotoSans-Regular.ttf");

/// Sheet export: one sheet — one page. OCGs are shared by the document, unique by name
/// (visibility is taken from the first sheet with such a layer). `font` — TrueType data;
/// `None` or an unusable font — Helvetica/WinAnsi.
pub fn export_pdf_with_font(
    sheets: &[Sheet],
    font: Option<&[u8]>,
) -> Result<Vec<u8>, super::PdfError> {
    export_pdf_with_fonts(sheets, font, &[])
}

/// The same with a fallback font: a character comes from the primary, and if it has no glyph there — from the fallback.
pub fn export_pdf_with_fonts(
    sheets: &[Sheet],
    font: Option<&[u8]>,
    fallbacks_in: &[&[u8]],
) -> Result<Vec<u8>, super::PdfError> {
    validate(sheets)?;
    let used = used_chars(sheets);
    let fs = font.and_then(|f| {
        let primary = FontSubset::build(f, used.iter().copied())?;
        let mut remaining: Vec<char> = {
            let face = ttf_parser::Face::parse(f, 0).ok()?;
            used.iter()
                .copied()
                .filter(|&c| face.glyph_index(c).is_none())
                .collect()
        };
        let mut fallbacks = Vec::new();
        for fb in fallbacks_in {
            let Ok(face) = ttf_parser::Face::parse(fb, 0) else {
                continue;
            };
            let (mine, rest): (Vec<char>, Vec<char>) = remaining
                .iter()
                .partition(|&&c| face.glyph_index(c).is_some());
            remaining = rest;
            if !mine.is_empty() {
                fallbacks.extend(FontSubset::build(fb, mine));
            }
        }
        Some(Fonts { primary, fallbacks })
    });
    let mut ocgs: Vec<(String, bool)> = Vec::new();
    for sh in sheets {
        for l in &sh.layers {
            if !ocgs.iter().any(|(n, _)| *n == l.name) {
                ocgs.push((l.name.clone(), l.visible));
            }
        }
    }
    // Objects: 1 catalog, 2 pages, 3 font, 4.. OCG, then page/content pairs.
    let ocg_base = 4usize;
    // document images (without repeats) lie between the OCGs and the pages
    let mut doc_images: Vec<Arc<PdfImage>> = Vec::new();
    for sh in sheets {
        for im in sheet_images(sh) {
            if !doc_images.iter().any(|x| **x == *im) {
                doc_images.push(im);
            }
        }
    }
    let image_objs: usize = doc_images.iter().map(|i| image_objects(i)).sum();
    let page_base = ocg_base + ocgs.len() + image_objs;
    let font_base = page_base + 2 * sheets.len();

    let mut out: Vec<u8> = Vec::new();
    let mut offs: Vec<usize> = Vec::new();
    out.extend_from_slice(b"%PDF-1.5\n%\xE2\xE3\xCF\xD3\n");

    begin(&mut out, &mut offs);
    out.extend_from_slice(b"<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [");
    for i in 0..ocgs.len() {
        write!(out, "{} 0 R ", ocg_base + i).unwrap();
    }
    out.extend_from_slice(b"] /D << /Order [");
    for i in 0..ocgs.len() {
        write!(out, "{} 0 R ", ocg_base + i).unwrap();
    }
    out.extend_from_slice(b"] /OFF [");
    for (i, (_, vis)) in ocgs.iter().enumerate() {
        if !vis {
            write!(out, "{} 0 R ", ocg_base + i).unwrap();
        }
    }
    out.extend_from_slice(b"] >> >> >>");
    end(&mut out);

    begin(&mut out, &mut offs);
    write!(out, "<< /Type /Pages /Count {} /Kids [", sheets.len()).unwrap();
    for i in 0..sheets.len() {
        write!(out, "{} 0 R ", page_base + 2 * i).unwrap();
    }
    out.extend_from_slice(b"] >>");
    end(&mut out);

    begin(&mut out, &mut offs);
    if fs.is_some() {
        type0_body(&mut out, "AAAAAA+NotoSans", font_base);
    } else {
        out.extend_from_slice(
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        );
    }
    end(&mut out);

    for (name, _) in &ocgs {
        begin(&mut out, &mut offs);
        out.extend_from_slice(b"<< /Type /OCG /Name ");
        write_utf16_hex(&mut out, name);
        out.extend_from_slice(b" >>");
        end(&mut out);
    }

    let mut image_ids: Vec<usize> = Vec::with_capacity(doc_images.len());
    for im in &doc_images {
        image_ids.push(write_image(&mut out, &mut offs, im));
    }

    for (i, sh) in sheets.iter().enumerate() {
        let content_id = page_base + 2 * i + 1;
        begin(&mut out, &mut offs);
        out.extend_from_slice(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ");
        num(&mut out, sh.width_mm * PT_PER_MM);
        num(&mut out, sh.height_mm * PT_PER_MM);
        let mut f2 = String::new();
        for k in 0..fs.as_ref().map_or(0, |f| f.fallbacks.len()) {
            f2 += &format!("/F{} {} 0 R ", k + 2, font_base + 4 + 5 * k);
        }
        write!(
            out,
            "] /Resources << /Font << /F1 3 0 R {f2}>> /Properties << "
        )
        .unwrap();
        for (li, l) in sh.layers.iter().enumerate() {
            let g = ocgs.iter().position(|(n, _)| *n == l.name).unwrap();
            write!(out, "/L{} {} 0 R ", li, ocg_base + g).unwrap();
        }
        out.extend_from_slice(b">> ");
        let used = sheet_images(sh);
        if !used.is_empty() {
            out.extend_from_slice(b"/XObject << ");
            for (k, im) in used.iter().enumerate() {
                let g = doc_images.iter().position(|x| **x == **im).unwrap();
                write!(out, "/Im{k} {} 0 R ", image_ids[g]).unwrap();
            }
            out.extend_from_slice(b">> ");
        }
        write!(out, ">> /Contents {} 0 R >>", content_id).unwrap();
        end(&mut out);

        let c = content(sh, fs.as_ref());
        begin(&mut out, &mut offs);
        write_stream(&mut out, "", &c);
        end(&mut out);
    }

    if let Some(f) = &fs {
        font_objects(
            &mut out,
            &mut offs,
            &f.primary,
            "AAAAAA+NotoSans",
            font_base,
        );
        for (k, fb) in f.fallbacks.iter().enumerate() {
            let base = font_base + 4 + 5 * k;
            let name = format!("AAAAA{}+NotoSansFallback", (b'B' + k as u8) as char);
            begin(&mut out, &mut offs);
            type0_body(&mut out, &name, base + 1);
            end(&mut out);
            font_objects(&mut out, &mut offs, fb, &name, base + 1);
        }
    }

    let xref = out.len();
    write!(out, "xref\n0 {}\n0000000000 65535 f \n", offs.len() + 1).unwrap();
    for off in &offs {
        writeln!(out, "{:010} 00000 n ", off).unwrap();
    }
    write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
        offs.len() + 1,
        xref
    )
    .unwrap();
    Ok(out)
}

pub(super) fn begin(out: &mut Vec<u8>, offs: &mut Vec<usize>) {
    offs.push(out.len());
    writeln!(out, "{} 0 obj", offs.len()).unwrap();
}

pub(super) fn end(out: &mut Vec<u8>) {
    out.extend_from_slice(b"\nendobj\n");
}

pub(super) fn type0_body(out: &mut Vec<u8>, name: &str, base: usize) {
    write!(
        out,
        "<< /Type /Font /Subtype /Type0 /BaseFont /{name} /Encoding /Identity-H \
         /DescendantFonts [{base} 0 R] /ToUnicode {} 0 R >>",
        base + 3
    )
    .unwrap();
}

/// CIDFont, descriptor, font file and ToUnicode: objects base..base+3.
pub(super) fn font_objects(
    out: &mut Vec<u8>,
    offs: &mut Vec<usize>,
    f: &FontSubset,
    name: &str,
    base: usize,
) {
    begin(out, offs);
    write!(
        out,
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{name} \
         /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
         /FontDescriptor {} 0 R /CIDToGIDMap /Identity /DW 0 /W {} >>",
        base + 1,
        f.w_array()
    )
    .unwrap();
    end(out);

    begin(out, offs);
    write!(
        out,
        "<< /Type /FontDescriptor /FontName /{name} /Flags 4 \
         /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {} /Descent {} \
         /CapHeight {} /StemV 80 /FontFile2 {} 0 R >>",
        f.bbox[0],
        f.bbox[1],
        f.bbox[2],
        f.bbox[3],
        f.ascent,
        f.descent,
        f.cap_height,
        base + 2
    )
    .unwrap();
    end(out);

    begin(out, offs);
    write_stream(out, &format!("/Length1 {} ", f.data.len()), &f.data);
    end(out);

    begin(out, offs);
    write_stream(out, "", &f.to_unicode_cmap());
    end(out);
}
