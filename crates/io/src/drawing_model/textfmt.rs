//! Text details an underlay text carries beyond position and string: justification, style,
//! width factor, slant, and for multi-line text the attachment point, frame width, line spacing
//! and the raw content with its formatting codes. Read from TEXT/MTEXT, written back by the export.
// the write-back half lives only with the test-only DXF writer (`dxf-write`)
#![cfg_attr(not(any(test, feature = "dxf-write")), allow(dead_code, unused_imports))]

use crate::dxf::{mtext_plain, Common, Entity, MText, Pair, Text};

use super::source::UText;
#[cfg(any(test, feature = "dxf-write"))]
use super::write::Placement;

type P = (f64, f64);

/// All zero / `None` is a plain left-aligned single-line text in the default style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextFmt {
    /// Multi-line text (MTEXT) instead of a single line.
    pub mtext: bool,
    /// Horizontal justification, code 72 (0 left, 1 centre, 2 right, 3 aligned, 4 middle, 5 fit).
    pub halign: u8,
    /// Vertical justification, code 73 (0 baseline, 1 bottom, 2 middle, 3 top).
    pub valign: u8,
    /// Second alignment point relative to the text position (m); with `halign`/`valign` set.
    pub align_off: Option<P>,
    /// Text style name.
    pub style: Option<String>,
    /// Width factor; 0 when not given (1 in the file).
    pub width_factor: f64,
    /// Slant (degrees).
    pub oblique: f64,
    /// MTEXT attachment point 1..9; 0 when not given.
    pub attach: u8,
    /// MTEXT reference frame width (m); 0 for none.
    pub box_width: f64,
    /// MTEXT line spacing factor; 0 when not given.
    pub line_spacing: f64,
    /// MTEXT content with its formatting codes, as in the file.
    pub raw: Option<String>,
}

fn find(extra: &[Pair], code: i32) -> Option<f64> {
    extra
        .iter()
        .find(|p| p.code == code)
        .and_then(|p| p.value.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

impl TextFmt {
    /// Format of a TEXT; `unit` is metres per drawing unit.
    pub(super) fn from_text(t: &Text, unit: f64) -> TextFmt {
        let ex = &t.common.extra;
        let halign = find(ex, 72).map_or(0, |v| v.clamp(0.0, 5.0) as u8);
        let valign = find(ex, 73).map_or(0, |v| v.clamp(0.0, 3.0) as u8);
        let align_off = if halign != 0 || valign != 0 {
            match (find(ex, 11), find(ex, 21)) {
                (Some(x), Some(y)) => Some(((x - t.pos[0]) * unit, (y - t.pos[1]) * unit)),
                _ => None,
            }
        } else {
            None
        };
        TextFmt {
            halign,
            valign,
            align_off,
            style: t.style.clone(),
            width_factor: find(ex, 41).filter(|w| *w > 0.0).unwrap_or(0.0),
            oblique: find(ex, 51).unwrap_or(0.0),
            ..Default::default()
        }
    }

    /// Format of an MTEXT.
    pub(super) fn from_mtext(t: &MText, unit: f64) -> TextFmt {
        let w = t.width * unit;
        TextFmt {
            mtext: true,
            style: t.style.clone(),
            attach: t.attach.map_or(0, |a| a.clamp(0, 9) as u8),
            box_width: if w.is_finite() && w > 0.0 { w } else { 0.0 },
            line_spacing: find(&t.common.extra, 44).filter(|v| *v > 0.0).unwrap_or(0.0),
            raw: Some(t.text.clone()),
            ..Default::default()
        }
    }

    /// The text turns by an angle with this sine and cosine about its position.
    pub(super) fn rotate(&mut self, s: f64, c: f64) {
        if let Some(o) = self.align_off.as_mut() {
            *o = (c * o.0 - s * o.1, s * o.0 + c * o.1);
        }
    }

    /// The plan is mapped by the linear part `m` (a uniform scale `k` times a turn or mirror).
    pub(super) fn linear(&mut self, m: [f64; 4], k: f64) {
        if let Some(o) = self.align_off.as_mut() {
            *o = (m[0] * o.0 + m[1] * o.1, m[2] * o.0 + m[3] * o.1);
        }
        self.box_width *= k;
    }
}

/// MTEXT content for a text: the file's own content while the string is untouched, else the
/// string encoded (backslash, braces, line breaks, percent signs).
fn mtext_content(t: &UText) -> String {
    if let Some(raw) = &t.fmt.raw {
        if mtext_plain(raw) == t.text {
            return raw.clone();
        }
    }
    let src = if t.text.contains("%%") {
        t.text.replace('%', "%%%")
    } else {
        t.text.clone()
    };
    let mut out = String::with_capacity(src.len() + 8);
    let mut it = src.chars().peekable();
    while let Some(ch) = it.next() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\r' => {
                if it.peek() != Some(&'\n') {
                    out.push_str("\\P");
                }
            }
            '\n' => out.push_str("\\P"),
            c => out.push(c),
        }
    }
    out
}

fn push_num(extra: &mut Vec<Pair>, code: i32, v: f64) {
    extra.push(Pair::new(code, v.to_string()));
}

/// The TEXT or MTEXT entity of an underlay text. `text` is the string with `%` already escaped
/// for TEXT. A text without format details goes out as a plain TEXT in no style.
#[cfg(any(test, feature = "dxf-write"))]
pub(super) fn entity(t: &UText, pl: &Placement, mut common: Common, text: String) -> Entity {
    let f = &t.fmt;
    let pos = pl.pt(t.pos);
    let height = pl.len(t.height);
    let rotation = pl.deg(t.rotation);
    if f.mtext {
        if f.line_spacing > 0.0 {
            push_num(&mut common.extra, 44, f.line_spacing);
        }
        return Entity::MText(MText {
            common,
            pos,
            height,
            width: if f.box_width > 0.0 { pl.len(f.box_width) } else { 0.0 },
            text: mtext_content(t),
            rotation,
            style: f.style.clone(),
            attach: (f.attach > 0).then_some(i32::from(f.attach)),
        });
    }
    if f.width_factor > 0.0 && (f.width_factor - 1.0).abs() > 1e-9 {
        push_num(&mut common.extra, 41, f.width_factor);
    }
    if f.oblique != 0.0 {
        push_num(&mut common.extra, 51, f.oblique);
    }
    if f.halign != 0 {
        common.extra.push(Pair::new(72, f.halign.to_string()));
    }
    if f.halign != 0 || f.valign != 0 {
        let off = f.align_off.unwrap_or((0.0, 0.0));
        let a = pl.pt((t.pos.0 + off.0, t.pos.1 + off.1));
        push_num(&mut common.extra, 11, a[0]);
        push_num(&mut common.extra, 21, a[1]);
        push_num(&mut common.extra, 31, a[2]);
    }
    if f.valign != 0 {
        common.extra.push(Pair::new(73, f.valign.to_string()));
    }
    Entity::Text(Text {
        common,
        pos,
        height,
        text,
        rotation,
        style: f.style.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::source::{UText, Underlay};
    use super::super::write::{to_dxf, Placement};
    use crate::dxf::{self, Common, DxfDocument, Entity, MText, Pair, Text, TextStyle};

    const UNIT: f64 = 0.001;

    fn with_extra(pairs: &[(i32, &str)]) -> Common {
        let mut c = Common::on_layer("A-TEXT");
        c.extra = pairs.iter().map(|(k, v)| Pair::new(*k, *v)).collect();
        c
    }

    fn doc_of(e: Entity) -> DxfDocument {
        let mut d = DxfDocument::default();
        d.styles.push(TextStyle {
            name: "Romans".into(),
            flags: 0,
            height: 0.0,
            width_factor: 0.9,
            oblique: 0.0,
            font: "romans.shx".into(),
            big_font: String::new(),
            extra: vec![],
        });
        d.entities.push(e);
        d
    }

    /// Underlay -> file text -> file read back -> underlay.
    fn round(doc: &DxfDocument) -> (Underlay, DxfDocument, Underlay) {
        let u1 = Underlay::from_dxf(doc, UNIT);
        let (out, _) = to_dxf(&u1, &Placement::file(UNIT));
        let out = dxf::read_str(&dxf::write_string(&out)).expect("written text reads back");
        let u2 = Underlay::from_dxf(&out, UNIT);
        (u1, out, u2)
    }

    /// Values rounded to a micrometre: a file round trip may move the last digits.
    fn norm(t: &UText) -> UText {
        let r = |v: f64| (v * 1e6).round() / 1e6;
        let mut t = t.clone();
        t.pos = (r(t.pos.0), r(t.pos.1));
        t.height = r(t.height);
        t.fmt.box_width = r(t.fmt.box_width);
        t.fmt.oblique = r(t.fmt.oblique);
        t.fmt.width_factor = r(t.fmt.width_factor);
        t.fmt.align_off = t.fmt.align_off.map(|o| (r(o.0), r(o.1)));
        t
    }

    fn num(c: &Common, code: i32) -> f64 {
        c.extra
            .iter()
            .find(|p| p.code == code)
            .unwrap_or_else(|| panic!("code {code} missing"))
            .value
            .trim()
            .parse()
            .unwrap()
    }

    fn text_of(doc: &DxfDocument) -> &Text {
        let Entity::Text(t) = &doc.entities[0] else {
            panic!("TEXT expected")
        };
        t
    }

    fn mtext_of(doc: &DxfDocument) -> &MText {
        let Entity::MText(t) = &doc.entities[0] else {
            panic!("MTEXT expected")
        };
        t
    }

    fn text(extra: &[(i32, &str)], style: Option<&str>) -> Entity {
        Entity::Text(Text {
            common: with_extra(extra),
            pos: [1000.0, 2000.0, 0.0],
            height: 250.0,
            text: "Room".into(),
            rotation: 0.0,
            style: style.map(String::from),
        })
    }

    fn mtext(s: &str, attach: i32) -> Entity {
        Entity::MText(MText {
            common: with_extra(&[(44, "1.5")]),
            pos: [1000.0, 2000.0, 0.0],
            height: 200.0,
            width: 3000.0,
            text: s.into(),
            rotation: 0.0,
            style: Some("Romans".into()),
            attach: Some(attach),
        })
    }

    #[test]
    fn edited_dwg_export_keeps_centered_text() {
        let doc = doc_of(text(&[(72, "1"), (11, "1500"), (21, "2100"), (31, "0")], None));
        let (u1, out, u2) = round(&doc);
        assert_eq!(u1.texts[0].fmt.halign, 1);
        let o = u1.texts[0].fmt.align_off.unwrap();
        assert!((o.0 - 0.5).abs() < 1e-9 && (o.1 - 0.1).abs() < 1e-9);
        let t = text_of(&out);
        assert_eq!(num(&t.common, 72), 1.0);
        assert!((num(&t.common, 11) - 1500.0).abs() < 1e-6);
        assert!((num(&t.common, 21) - 2100.0).abs() < 1e-6);
        assert_eq!(norm(&u2.texts[0]), norm(&u1.texts[0]));
    }

    #[test]
    fn right_aligned_text_with_vertical_justification() {
        let doc = doc_of(text(
            &[(72, "2"), (73, "3"), (11, "900"), (21, "2300"), (31, "0")],
            None,
        ));
        let (u1, out, u2) = round(&doc);
        assert_eq!((u1.texts[0].fmt.halign, u1.texts[0].fmt.valign), (2, 3));
        let t = text_of(&out);
        assert_eq!((num(&t.common, 72), num(&t.common, 73)), (2.0, 3.0));
        assert!((num(&t.common, 11) - 900.0).abs() < 1e-6);
        assert_eq!(norm(&u2.texts[0]), norm(&u1.texts[0]));
    }

    #[test]
    fn mtext_multiline_roundtrip() {
        let doc = doc_of(mtext("{\\fArial|b1;First}\\Psecond line\\P100%%d", 5));
        let (u1, out, u2) = round(&doc);
        let f = &u1.texts[0].fmt;
        assert!(f.mtext && f.attach == 5 && f.line_spacing == 1.5);
        assert!((f.box_width - 3.0).abs() < 1e-9);
        let m = mtext_of(&out);
        assert_eq!(m.text, "{\\fArial|b1;First}\\Psecond line\\P100%%d");
        assert_eq!((m.attach, m.style.as_deref()), (Some(5), Some("Romans")));
        assert!((m.width - 3000.0).abs() < 1e-6);
        assert!((num(&m.common, 44) - 1.5).abs() < 1e-9);
        assert_eq!(norm(&u2.texts[0]), norm(&u1.texts[0]));
    }

    #[test]
    fn long_mtext_survives_chunking() {
        let body: String = (0..120).map(|i| format!("word{i}\\P")).collect();
        assert!(body.chars().count() > 600);
        let doc = doc_of(mtext(&body, 1));
        let (_, out, u2) = round(&doc);
        assert_eq!(mtext_of(&out).text, body);
        assert_eq!(u2.texts[0].fmt.raw.as_deref(), Some(body.as_str()));
        assert!(dxf::write_string(&out).matches("\n3\n").count() >= 2);
    }

    #[test]
    fn edited_mtext_is_encoded() {
        let doc = doc_of(mtext("old\\Ptext", 7));
        let mut u = Underlay::from_dxf(&doc, UNIT);
        u.texts[0].text = "a\\b {c}\nsecond".into();
        let (out, _) = to_dxf(&u, &Placement::file(UNIT));
        let out = dxf::read_str(&dxf::write_string(&out)).unwrap();
        let m = mtext_of(&out);
        assert_eq!(m.text, "a\\\\b \\{c\\}\\Psecond");
        assert_eq!(m.attach, Some(7));
        let u2 = Underlay::from_dxf(&out, UNIT);
        assert_eq!(u2.texts[0].text, "a\\b {c} second");
    }

    #[test]
    fn text_style_kept_on_export() {
        let doc = doc_of(text(&[(41, "0.8"), (51, "15")], Some("Romans")));
        let (u1, out, u2) = round(&doc);
        assert_eq!(u1.texts[0].fmt.style.as_deref(), Some("Romans"));
        let t = text_of(&out);
        assert_eq!(t.style.as_deref(), Some("Romans"));
        assert!((num(&t.common, 41) - 0.8).abs() < 1e-9);
        assert!((num(&t.common, 51) - 15.0).abs() < 1e-9);
        let st = out.styles.iter().find(|s| s.name == "Romans").expect("style entry");
        assert_eq!(st.font, "romans.shx");
        assert_eq!(norm(&u2.texts[0]), norm(&u1.texts[0]));
    }

    #[test]
    fn plain_text_writes_as_before() {
        let doc = doc_of(text(&[], None));
        let (u1, out, _) = round(&doc);
        assert_eq!(u1.texts[0].fmt, super::TextFmt::default());
        let t = text_of(&out);
        assert!(t.style.is_none() && t.common.extra.is_empty());
    }

    #[test]
    fn alignment_offset_follows_moves_and_placement() {
        let doc = doc_of(text(&[(72, "1"), (11, "1500"), (21, "2000"), (31, "0")], None));
        let mut u = Underlay::from_dxf(&doc, UNIT);
        u.texts[0].fmt.rotate(1.0, 0.0);
        let o = u.texts[0].fmt.align_off.unwrap();
        assert!(o.0.abs() < 1e-12 && (o.1 - 0.5).abs() < 1e-12);
        let pl = Placement {
            offset: (0.0, 0.0),
            rot: 0.0,
            unit: UNIT,
        };
        let (out, _) = to_dxf(&u, &pl);
        let t = text_of(&out);
        assert!((num(&t.common, 11) - 1000.0).abs() < 1e-6);
        assert!((num(&t.common, 21) - 2500.0).abs() < 1e-6);
    }
}
