// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A TrueType font subset for Type0/CIDFontType2 (Identity-H) with ToUnicode.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use subsetter::GlyphRemapper;

pub struct FontSubset {
    /// TrueType with only the used glyphs; CID = new GID.
    pub data: Vec<u8>,
    pub char_cid: BTreeMap<char, u16>,
    /// (cid, width in 1/1000 em), in ascending cid order.
    pub widths: Vec<(u16, u32)>,
    pub bbox: [i32; 4],
    pub ascent: i32,
    pub descent: i32,
    pub cap_height: i32,
}

impl FontSubset {
    pub fn build(font: &[u8], chars: impl IntoIterator<Item = char>) -> Option<FontSubset> {
        let mut remap = GlyphRemapper::new();
        let mut s = FontSubset::empty(font)?;
        s.grow(font, &mut remap, chars)?;
        s.seal(font, &remap)?;
        Some(s)
    }

    /// Font metrics without glyphs: the start of a subset that grows as pages are written.
    pub fn empty(font: &[u8]) -> Option<FontSubset> {
        let face = ttf_parser::Face::parse(font, 0).ok()?;
        let upem = face.units_per_em().max(1) as f64;
        let k = |v: f64| (v * 1000.0 / upem).round() as i32;
        let bb = face.global_bounding_box();
        let asc = k(face.ascender() as f64);
        Some(FontSubset {
            data: vec![],
            char_cid: BTreeMap::new(),
            widths: vec![],
            bbox: [
                k(bb.x_min as f64),
                k(bb.y_min as f64),
                k(bb.x_max as f64),
                k(bb.y_max as f64),
            ],
            ascent: asc,
            descent: k(face.descender() as f64),
            cap_height: face.capital_height().map_or(asc, |h| k(h as f64)),
        })
    }

    /// Adds characters. CIDs already issued do not change: `remap` numbers glyphs by first appearance.
    pub fn grow(
        &mut self,
        font: &[u8],
        remap: &mut GlyphRemapper,
        chars: impl IntoIterator<Item = char>,
    ) -> Option<()> {
        let face = ttf_parser::Face::parse(font, 0).ok()?;
        let upem = face.units_per_em().max(1) as f64;
        for c in chars {
            if self.char_cid.contains_key(&c) {
                continue;
            }
            let g = face.glyph_index(c).map_or(0, |g| g.0);
            let cid = remap.remap(g);
            self.char_cid.insert(c, cid);
            if let Err(i) = self.widths.binary_search_by_key(&cid, |w| w.0) {
                let adv = face.glyph_hor_advance(ttf_parser::GlyphId(g)).unwrap_or(0);
                let w = (adv as f64 * 1000.0 / upem).round() as i32;
                self.widths.insert(i, (cid, w.max(0) as u32));
            }
        }
        Some(())
    }

    /// The font file with the accumulated glyphs.
    pub fn seal(&mut self, font: &[u8], remap: &GlyphRemapper) -> Option<()> {
        self.data = subsetter::subset(font, 0, remap).ok()?;
        Some(())
    }

    /// A string as hex glyphs (2 bytes per character).
    pub fn encode(&self, s: &str) -> String {
        let mut o = String::from("<");
        for c in s.chars() {
            let cid = self.char_cid.get(&c).copied().unwrap_or(0);
            write!(o, "{:04X}", cid).unwrap();
        }
        o.push('>');
        o
    }

    pub fn to_unicode_cmap(&self) -> Vec<u8> {
        let mut by_cid: BTreeMap<u16, char> = BTreeMap::new();
        for (c, cid) in &self.char_cid {
            by_cid.entry(*cid).or_insert(*c);
        }
        let mut s = String::from(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
             /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
             /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
             1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
        );
        let all: Vec<_> = by_cid.into_iter().filter(|(cid, _)| *cid != 0).collect();
        for chunk in all.chunks(100) {
            writeln!(s, "{} beginbfchar", chunk.len()).unwrap();
            for (cid, c) in chunk {
                write!(s, "<{:04X}> <", cid).unwrap();
                let mut b = [0u16; 2];
                for u in c.encode_utf16(&mut b) {
                    write!(s, "{:04X}", u).unwrap();
                }
                s.push_str(">\n");
            }
            s.push_str("endbfchar\n");
        }
        s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
        s.into_bytes()
    }

    pub fn w_array(&self) -> String {
        let mut s = String::from("[");
        for (cid, w) in &self.widths {
            write!(s, "{} [{}] ", cid, w).unwrap();
        }
        s.push(']');
        s
    }
}
