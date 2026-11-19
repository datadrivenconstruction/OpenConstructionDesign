//! Streamed multi-page PDF: a page is compressed and written to the file at once, only object
//! offsets and the character set stay in memory. The font is built up on the fly: CIDs are issued on a glyph's
//! first appearance and never change, so pages already written stay valid; the font subset,
//! page tree and catalog are written at the end.

use std::io::Write;

use subsetter::GlyphRemapper;

use super::font::FontSubset;
use super::writer::{
    begin, cjk_fallback_fonts, content, end, font_objects, num, sheet_images, type0_body,
    used_chars, validate, write_image, write_stream, write_utf16_hex, Fonts, EMBEDDED_FONT, PT_PER_MM,
};
use super::{PdfError, PdfImage, Sheet};
use std::sync::Arc;

const CATALOG: usize = 1;
const PAGES: usize = 2;
const F1: usize = 3;
const F1_NAME: &str = "AAAAAA+NotoSans";

/// A fallback font already used in the document.
struct Fallback {
    /// Index in the candidate list.
    source: usize,
    remap: GlyphRemapper,
    type0: usize,
}

pub struct PdfBook<W: Write> {
    w: W,
    pos: usize,
    /// Object offset by number − 1; 0 — the number is reserved, the object will be written later.
    offs: Vec<usize>,
    pages: Vec<usize>,
    /// (name, visibility, object).
    ocgs: Vec<(String, bool, usize)>,
    fonts: Option<Fonts>,
    remap: GlyphRemapper,
    fallbacks: Vec<Fallback>,
    candidates: Option<Vec<Vec<u8>>>,
    /// Images already written to the file, and their object numbers.
    images: Vec<(Arc<PdfImage>, usize)>,
    broken: bool,
}

fn io_err(e: std::io::Error) -> PdfError {
    PdfError(e.to_string())
}

impl<W: Write> PdfBook<W> {
    pub fn new(mut w: W) -> Result<Self, PdfError> {
        let head = b"%PDF-1.5\n%\xE2\xE3\xCF\xD3\n";
        w.write_all(head).map_err(io_err)?;
        Ok(PdfBook {
            w,
            pos: head.len(),
            offs: vec![0; F1],
            pages: vec![],
            ocgs: vec![],
            fonts: FontSubset::empty(EMBEDDED_FONT).map(|primary| Fonts {
                primary,
                fallbacks: vec![],
            }),
            remap: GlyphRemapper::new(),
            fallbacks: vec![],
            candidates: None,
            images: vec![],
            broken: false,
        })
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Writing to the file failed: the document can no longer be assembled, unlike a single-page error.
    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// Page characters: the primary font first, whatever it lacks — the first suitable fallback.
    fn add_chars(&mut self, sh: &Sheet) {
        let Some(f) = &mut self.fonts else {
            return;
        };
        let used = used_chars(std::slice::from_ref(sh));
        let _ = f
            .primary
            .grow(EMBEDDED_FONT, &mut self.remap, used.iter().copied());
        let has = |s: &FontSubset, c: char| s.char_cid.get(&c).is_some_and(|&g| g != 0);
        let mut rest: Vec<char> = used
            .iter()
            .copied()
            .filter(|&c| {
                !c.is_whitespace()
                    && !c.is_control()
                    && !has(&f.primary, c)
                    && !f.fallbacks.iter().any(|s| has(s, c))
            })
            .collect();
        if rest.is_empty() {
            return;
        }
        let candidates = self.candidates.get_or_insert_with(cjk_fallback_fonts);
        for (source, bytes) in candidates.iter().enumerate() {
            if rest.is_empty() {
                break;
            }
            let Ok(face) = ttf_parser::Face::parse(bytes, 0) else {
                continue;
            };
            let (mine, other): (Vec<char>, Vec<char>) =
                rest.iter().partition(|&&c| face.glyph_index(c).is_some());
            rest = other;
            if mine.is_empty() {
                continue;
            }
            let k = match self.fallbacks.iter().position(|s| s.source == source) {
                Some(k) => k,
                None => {
                    let Some(sub) = FontSubset::empty(bytes) else {
                        continue;
                    };
                    f.fallbacks.push(sub);
                    self.offs.push(0);
                    self.fallbacks.push(Fallback {
                        source,
                        remap: GlyphRemapper::new(),
                        type0: self.offs.len(),
                    });
                    self.fallbacks.len() - 1
                }
            };
            let _ = f.fallbacks[k].grow(bytes, &mut self.fallbacks[k].remap, mine);
        }
    }

    /// Buffer with objects from index `first`: offsets in it become file offsets.
    fn flush(&mut self, buf: &[u8], first: usize) -> Result<(), PdfError> {
        let pos = self.pos;
        for o in &mut self.offs[first..] {
            *o += pos;
        }
        self.write(buf)
    }

    fn write(&mut self, buf: &[u8]) -> Result<(), PdfError> {
        if let Err(e) = self.w.write_all(buf) {
            self.broken = true;
            return Err(io_err(e));
        }
        self.pos += buf.len();
        Ok(())
    }

    /// An object with a pre-reserved number.
    fn object(&mut self, id: usize, body: &[u8]) -> Result<(), PdfError> {
        self.offs[id - 1] = self.pos;
        let mut buf = Vec::with_capacity(body.len() + 32);
        writeln!(buf, "{id} 0 obj").map_err(io_err)?;
        buf.extend_from_slice(body);
        end(&mut buf);
        self.write(&buf)
    }

    /// A page goes to the file whole; a sheet data error leaves no trace in the file.
    pub fn add_page(&mut self, sh: &Sheet) -> Result<(), PdfError> {
        validate(std::slice::from_ref(sh))?;
        self.add_chars(sh);
        let mut layer_ids = Vec::with_capacity(sh.layers.len());
        for l in &sh.layers {
            let id = match self.ocgs.iter().find(|o| o.0 == l.name) {
                Some(o) => o.2,
                None => {
                    self.offs.push(0);
                    let id = self.offs.len();
                    self.ocgs.push((l.name.clone(), l.visible, id));
                    id
                }
            };
            layer_ids.push(id);
        }
        let c = content(sh, self.fonts.as_ref());
        let first = self.offs.len();
        let mut buf = Vec::with_capacity(c.len() / 2 + 512);
        let mut xobj = String::new();
        let mut fresh: Vec<(Arc<PdfImage>, usize)> = vec![];
        for (k, im) in sheet_images(sh).iter().enumerate() {
            let known = self
                .images
                .iter()
                .chain(&fresh)
                .find(|(x, _)| **x == **im)
                .map(|(_, id)| *id);
            let id = match known {
                Some(id) => id,
                None => {
                    let id = write_image(&mut buf, &mut self.offs, im);
                    fresh.push((im.clone(), id));
                    id
                }
            };
            xobj += &format!("/Im{k} {id} 0 R ");
        }
        let page_id = self.offs.len() + 1;
        begin(&mut buf, &mut self.offs);
        buf.extend_from_slice(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ");
        num(&mut buf, sh.width_mm * PT_PER_MM);
        num(&mut buf, sh.height_mm * PT_PER_MM);
        let mut f2 = String::new();
        for (k, s) in self.fallbacks.iter().enumerate() {
            f2 += &format!("/F{} {} 0 R ", k + 2, s.type0);
        }
        write!(
            buf,
            "] /Resources << /Font << /F1 {F1} 0 R {f2}>> /Properties << "
        )
        .map_err(io_err)?;
        for (li, g) in layer_ids.iter().enumerate() {
            write!(buf, "/L{li} {g} 0 R ").map_err(io_err)?;
        }
        if xobj.is_empty() {
            write!(buf, ">> >> /Contents {} 0 R >>", page_id + 1).map_err(io_err)?;
        } else {
            write!(buf, ">> /XObject << {xobj}>> >> /Contents {} 0 R >>", page_id + 1)
                .map_err(io_err)?;
        }
        end(&mut buf);
        begin(&mut buf, &mut self.offs);
        write_stream(&mut buf, "", &c);
        end(&mut buf);
        self.flush(&buf, first)?;
        self.images.extend(fresh);
        self.pages.push(page_id);
        Ok(())
    }

    /// Fonts, page tree, catalog, xref. Returns the writer: flushing and closing are up to the caller.
    pub fn finish(mut self) -> Result<W, PdfError> {
        if self.pages.is_empty() {
            return Err(PdfError("no pages".into()));
        }
        self.write_fonts()?;
        for (name, _, id) in self.ocgs.clone() {
            let mut b = b"<< /Type /OCG /Name ".to_vec();
            write_utf16_hex(&mut b, &name);
            b.extend_from_slice(b" >>");
            self.object(id, &b)?;
        }
        let mut pages = format!("<< /Type /Pages /Count {} /Kids [", self.pages.len());
        for p in &self.pages {
            pages += &format!("{p} 0 R ");
        }
        pages += "] >>";
        self.object(PAGES, pages.as_bytes())?;
        let refs: String = self.ocgs.iter().map(|o| format!("{} 0 R ", o.2)).collect();
        let off: String = self
            .ocgs
            .iter()
            .filter(|o| !o.1)
            .map(|o| format!("{} 0 R ", o.2))
            .collect();
        let cat = format!(
            "<< /Type /Catalog /Pages {PAGES} 0 R /OCProperties << /OCGs [{refs}] \
             /D << /Order [{refs}] /OFF [{off}] >> >> >>"
        );
        self.object(CATALOG, cat.as_bytes())?;

        let xref = self.pos;
        let mut tail = format!("xref\n0 {}\n0000000000 65535 f \n", self.offs.len() + 1);
        for off in &self.offs {
            tail += &format!("{off:010} 00000 n \n");
        }
        tail += &format!(
            "trailer\n<< /Size {} /Root {CATALOG} 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            self.offs.len() + 1
        );
        self.write(tail.as_bytes())?;
        Ok(self.w)
    }

    fn write_fonts(&mut self) -> Result<(), PdfError> {
        let Some(mut fonts) = self.fonts.take() else {
            return self.object(
                F1,
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
            );
        };
        let bad = || PdfError("font subset failed".into());
        fonts
            .primary
            .seal(EMBEDDED_FONT, &self.remap)
            .ok_or_else(bad)?;
        let first = self.offs.len();
        let base = first + 1;
        let mut buf = vec![];
        font_objects(&mut buf, &mut self.offs, &fonts.primary, F1_NAME, base);
        self.flush(&buf, first)?;
        let mut t0 = vec![];
        type0_body(&mut t0, F1_NAME, base);
        self.object(F1, &t0)?;
        let candidates = self.candidates.take().unwrap_or_default();
        for k in 0..fonts.fallbacks.len() {
            let (source, type0) = (self.fallbacks[k].source, self.fallbacks[k].type0);
            fonts.fallbacks[k]
                .seal(&candidates[source], &self.fallbacks[k].remap)
                .ok_or_else(bad)?;
            let name = format!("AAAAA{}+NotoSansFallback", (b'B' + k as u8) as char);
            let first = self.offs.len();
            let base = first + 1;
            let mut buf = vec![];
            font_objects(&mut buf, &mut self.offs, &fonts.fallbacks[k], &name, base);
            self.flush(&buf, first)?;
            let mut t0 = vec![];
            type0_body(&mut t0, &name, base);
            self.object(type0, &t0)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::import::PdfFile;
    use super::super::{Item, SheetLayer};
    use super::*;

    fn page(s: &str, layer: &str) -> Sheet {
        Sheet {
            width_mm: 210.0,
            height_mm: 297.0,
            layers: vec![SheetLayer {
                name: layer.into(),
                visible: true,
            }],
            items: vec![
                Item::Text {
                    pos: [20.0, 20.0],
                    height_mm: 3.0,
                    rotation: 0.0,
                    string: s.into(),
                    rgb: [0, 0, 0],
                    layer: 0,
                },
                Item::Polyline {
                    pts: vec![[0.0, 0.0], [10.0, 10.0]],
                    closed: false,
                    weight_mm: 0.25,
                    rgb: [0, 0, 0],
                    layer: 0,
                },
            ],
        }
    }

    #[test]
    fn pages_written_before_new_glyphs_keep_their_text() {
        let mut b = PdfBook::new(Vec::new()).unwrap();
        let ru = crate::ru_fixtures::ru;
        let world = format!("World {} 42", ru("cyr_zhe"));
        b.add_page(&page(ru("hello_word"), "A")).unwrap();
        // new glyphs after a written page: the CIDs of the first one do not shift
        b.add_page(&page(&world, "B")).unwrap();
        b.add_page(&page(ru("hello_word"), "A")).unwrap();
        assert_eq!(b.page_count(), 3);
        let bytes = b.finish().unwrap();
        let pdf = PdfFile::parse(&bytes).unwrap();
        assert_eq!(pdf.page_count(), 3);
        let text = |k: usize| pdf.page(k).unwrap().texts[0].string.clone();
        assert_eq!(text(0), ru("hello_word"));
        assert_eq!(text(1), world);
        assert_eq!(text(2), ru("hello_word"));
    }

    #[test]
    fn bad_page_leaves_no_trace_and_empty_book_is_an_error() {
        let mut b = PdfBook::new(Vec::new()).unwrap();
        let mut bad = page("x", "A");
        bad.width_mm = f64::NAN;
        assert!(b.add_page(&bad).is_err());
        assert!(!b.is_broken());
        assert_eq!(b.page_count(), 0);
        assert!(PdfBook::new(Vec::new()).unwrap().finish().is_err());
        b.add_page(&page("ok", "A")).unwrap();
        let pdf = PdfFile::parse(&b.finish().unwrap()).unwrap();
        assert_eq!(pdf.page_count(), 1);
    }
}
