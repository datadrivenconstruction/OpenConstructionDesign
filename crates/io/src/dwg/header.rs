//! Header variables (only the ones the document needs) and the class table
//! (spec: docs/specs/DWG.md §4–5).

use super::bits::{le32, Bits};
use super::{DwgReport, Version, R};

const HEADER_SENTINEL: [u8; 16] = [
    0xCF, 0x7B, 0x1F, 0x23, 0xFD, 0xDE, 0x38, 0xA9, 0x5F, 0x7C, 0x68, 0xB8, 0x4E, 0x6D, 0x33, 0x5F,
];

/// Code page index for `crate::dxf::codepage::decode` from the stored code page number.
pub(crate) fn codepage_index(cp: u16) -> Option<usize> {
    match cp {
        28 => Some(0),
        29 => Some(1),
        30 => Some(2),
        32..=36 => Some(cp as usize - 29),
        38..=41 => Some(cp as usize - 30),
        _ => None,
    }
}

pub(crate) fn codepage_name(cp: u16) -> Option<&'static str> {
    const N: [&str; 12] = [
        "ANSI_1250",
        "ANSI_1251",
        "ANSI_1252",
        "ANSI_1253",
        "ANSI_1254",
        "ANSI_1255",
        "ANSI_1256",
        "ANSI_1257",
        "ANSI_932",
        "ANSI_936",
        "ANSI_949",
        "ANSI_950",
    ];
    codepage_index(cp).map(|i| N[i])
}

/// Three streams of one record: data, strings (R2007+) and handles.
pub(crate) struct Tri<'a> {
    pub m: Bits<'a>,
    pub s: Option<Bits<'a>>,
    pub h: Bits<'a>,
    /// Handles interleaved with data (header variables before R2007).
    pub h_inline: bool,
    pub unicode: bool,
    pub cp: Option<usize>,
}

impl Tri<'_> {
    pub fn tv(&mut self) -> R<String> {
        if self.unicode {
            match &mut self.s {
                Some(s) => s.tu(),
                None => Ok(String::new()),
            }
        } else {
            let raw = self.m.tv_raw()?;
            Ok(crate::dxf::codepage::decode(&raw, self.cp))
        }
    }

    pub fn h(&mut self) -> R<(u8, u64)> {
        if self.h_inline {
            self.m.h()
        } else {
            self.h.h()
        }
    }

    /// Color: index only before R2004; index, RGB word and optional names after.
    pub fn cmc(&mut self, ver: Version) -> R<(i32, Option<i32>)> {
        let idx = self.m.bs()? as i16 as i32;
        if ver < Version::R2004 {
            return Ok((idx, None));
        }
        let rgb = self.m.bl()?;
        let flags = self.m.rc()?;
        if flags & 1 != 0 {
            self.tv()?;
        }
        if flags & 2 != 0 {
            self.tv()?;
        }
        Ok(match rgb >> 24 {
            0xC2 => (idx, Some((rgb & 0xFF_FFFF) as i32)),
            0xC3 => ((rgb & 0xFF) as i32, None),
            0xC0 => (256, None),
            0xC1 => (0, None),
            _ => (idx, None),
        })
    }
}

/// Splits off the string stream that ends at `data_end` (flag bit, size word(s), strings).
/// Returns the end of the data stream and the string reader.
pub(crate) fn split_strings(d: &[u8], start: usize, data_end: usize) -> (usize, Option<Bits<'_>>) {
    let max = d.len() * 8;
    if data_end == 0 || data_end > max || data_end <= start {
        return (data_end.min(max), None);
    }
    let mut f = Bits::view(d, data_end - 1, data_end);
    if !f.b().unwrap_or(false) {
        return (data_end - 1, None);
    }
    if data_end < start + 17 {
        return (data_end - 1, None);
    }
    let mut top = data_end - 17;
    let Ok(mut size) = Bits::view(d, top, top + 16).rs().map(|v| v as usize) else {
        return (data_end - 1, None);
    };
    if size & 0x8000 != 0 {
        if top < start + 16 {
            return (data_end - 1, None);
        }
        top -= 16;
        let hi = Bits::view(d, top, top + 16).rs().unwrap_or(0) as usize;
        size = (size & 0x7FFF) | (hi << 15);
    }
    if size > top - start {
        return (data_end - 1, None);
    }
    (top - size, Some(Bits::view(d, top - size, top)))
}

/// Start byte of the bit data in a R2007+ framed section (an optional high size word).
fn unicode_base(d: &[u8], ver: Version) -> usize {
    if ver >= Version::R2010 && le32(d, 20).unwrap_or(1) == 0 {
        24
    } else {
        20
    }
}

/// Section framing: (data bits for the three streams, size end).
fn streams(d: &[u8], ver: Version, cp: Option<usize>) -> R<Tri<'_>> {
    let size = le32(d, 16)? as usize;
    if ver < Version::R2007 {
        let end = 20usize.saturating_add(size).saturating_mul(8);
        return Ok(Tri {
            m: Bits::view(d, 160, end),
            s: None,
            h: Bits::view(d, 0, 0),
            h_inline: true,
            unicode: false,
            cp,
        });
    }
    let base = unicode_base(d, ver);
    let bitsize = le32(d, base)? as usize;
    let end_bit = base * 8 + bitsize;
    let first = (base + 4) * 8;
    let (m_end, s) = split_strings(d, first, end_bit);
    Ok(Tri {
        m: Bits::view(d, first, m_end),
        s,
        h: Bits::view(d, end_bit, base.saturating_add(size).saturating_mul(8)),
        h_inline: false,
        unicode: true,
        cp,
    })
}

#[derive(Debug, Default, Clone)]
pub(crate) struct HeaderInfo {
    pub insbase: Option<[f64; 3]>,
    pub extmin: Option<[f64; 3]>,
    pub extmax: Option<[f64; 3]>,
    pub limmin: Option<[f64; 2]>,
    pub limmax: Option<[f64; 2]>,
    pub ltscale: Option<f64>,
    pub insunits: Option<u16>,
}

pub(crate) fn header(d: &[u8], ver: Version, cp: u16, rep: &mut DwgReport) -> HeaderInfo {
    let mut out = HeaderInfo::default();
    if d.get(..16) != Some(&HEADER_SENTINEL[..]) {
        rep.warn("header variables: start sentinel mismatch");
    }
    let mut t = match streams(d, ver, codepage_index(cp)) {
        Ok(t) => t,
        Err(e) => {
            rep.warn(format!("header variables: {}", e.0));
            return out;
        }
    };
    if let Err(e) = header_seq(&mut t, ver, &mut out) {
        rep.warn(format!("header variables partly read: {}", e.0));
    }
    // INSUNITS is the last of a long sequence: out of range means an upstream misparse.
    if let Some(u) = out.insunits.filter(|&u| u > 24) {
        rep.warn(format!(
            "header variables: INSUNITS {u} out of range, ignored"
        ));
        out.insunits = None;
    }
    out
}

fn header_seq(t: &mut Tri, v: Version, o: &mut HeaderInfo) -> R<()> {
    use Version::*;
    let old = v <= R14;
    if v >= R2013 {
        t.m.bll()?;
    }
    for _ in 0..4 {
        t.m.bd()?;
    }
    for _ in 0..4 {
        t.tv()?;
    }
    t.m.bl()?;
    t.m.bl()?;
    if old {
        t.m.bs()?;
    }
    if v <= R2000 {
        t.h()?;
    }
    let bits = |t: &mut Tri, n: usize| -> R<()> {
        for _ in 0..n {
            t.m.b()?;
        }
        Ok(())
    };
    let shorts = |t: &mut Tri, n: usize| -> R<()> {
        for _ in 0..n {
            t.m.bs()?;
        }
        Ok(())
    };
    bits(t, 2)?; // DIMASO DIMSHO
    if old {
        bits(t, 1)?; // DIMSAV
    }
    bits(t, 7)?; // PLINEGEN .. LIMCHECK
    if old {
        bits(t, 1)?; // BLIPMODE
    }
    if v >= R2004 {
        bits(t, 1)?;
    }
    bits(t, 4)?; // USRTIMER SKPOLY ANGDIR SPLFRAME
    if old {
        bits(t, 2)?; // ATTREQ ATTDIA
    }
    bits(t, 2)?; // MIRRTEXT WORLDVIEW
    if old {
        bits(t, 1)?; // WIREFRAME
    }
    bits(t, 3)?; // TILEMODE PLIMCHECK VISRETAIN
    if old {
        bits(t, 1)?; // DELOBJ
    }
    bits(t, 2)?; // DISPSILH PELLIPSE
    shorts(t, 1)?; // PROXYGRAPHICS
    if old {
        shorts(t, 1)?; // DRAGMODE
    }
    shorts(t, 5)?; // TREEDEPTH LUNITS LUPREC AUNITS AUPREC
    if old {
        shorts(t, 1)?; // OSMODE
    }
    shorts(t, 1)?; // ATTMODE
    if old {
        shorts(t, 1)?; // COORDS
    }
    shorts(t, 1)?; // PDMODE
    if old {
        shorts(t, 1)?; // PICKSTYLE
    }
    if v >= R2004 {
        for _ in 0..3 {
            t.m.bl()?;
        }
    }
    shorts(t, 19)?; // USERI1..5 SPLINESEGS SURF* SPLINETYPE SHADEDGE .. TEXTQLTY
    o.ltscale = Some(t.m.bd()?);
    for _ in 0..20 {
        t.m.bd()?; // TEXTSIZE .. CELTSCALE
    }
    t.tv()?; // MENU
    for _ in 0..4 {
        t.m.bl()?; // TDCREATE TDUPDATE
    }
    if v >= R2004 {
        for _ in 0..3 {
            t.m.bl()?;
        }
    }
    for _ in 0..4 {
        t.m.bl()?; // TDINDWG TDUSRTIMER
    }
    t.cmc(v)?; // CECOLOR
    t.m.h()?; // HANDSEED (always in the data stream)
    for _ in 0..3 {
        t.h()?; // CLAYER TEXTSTYLE CELTYPE
    }
    if v >= R2007 {
        t.h()?; // CMATERIAL
    }
    t.h()?; // DIMSTYLE
    t.h()?; // CMLSTYLE
    if v >= R2000 {
        t.m.bd()?; // PSVPSCALE
    }
    space(t, v, &mut HeaderInfo::default())?; // paper space
    space(t, v, o)?; // model space
    if v < R2000 {
        return Ok(());
    }
    t.tv()?; // DIMPOST
    t.tv()?; // DIMAPOST
    for _ in 0..9 {
        t.m.bd()?; // DIMSCALE .. DIMTM
    }
    if v >= R2007 {
        t.m.bd()?;
        t.m.bd()?;
        t.m.bs()?;
        t.cmc(v)?;
    }
    bits(t, 6)?; // DIMTOL .. DIMSE2
    shorts(t, 3)?; // DIMTAD DIMZIN DIMAZIN
    if v >= R2007 {
        shorts(t, 1)?; // DIMARCSYM
    }
    for _ in 0..8 {
        t.m.bd()?; // DIMTXT .. DIMGAP
    }
    t.m.bd()?; // DIMALTRND
    bits(t, 1)?; // DIMALT
    shorts(t, 1)?; // DIMALTD
    bits(t, 4)?; // DIMTOFL DIMSAH DIMTIX DIMSOXD
    for _ in 0..3 {
        t.cmc(v)?; // DIMCLRD DIMCLRE DIMCLRT
    }
    shorts(t, 11)?; // DIMADEC .. DIMJUST
    bits(t, 2)?; // DIMSD1 DIMSD2
    shorts(t, 4)?; // DIMTOLJ DIMTZIN DIMALTZ DIMALTTZ
    bits(t, 1)?; // DIMUPT
    shorts(t, 1)?; // DIMATFIT
    if v >= R2007 {
        bits(t, 1)?; // DIMFXLON
    }
    if v >= R2010 {
        bits(t, 1)?;
        t.m.bd()?;
        t.tv()?;
        t.m.bd()?;
        t.tv()?;
    }
    for _ in 0..5 {
        t.h()?; // DIMTXSTY DIMLDRBLK DIMBLK DIMBLK1 DIMBLK2
    }
    if v >= R2007 {
        for _ in 0..3 {
            t.h()?;
        }
    }
    shorts(t, 2)?; // DIMLWD DIMLWE
    for _ in 0..9 {
        t.h()?; // table controls
    }
    if v <= R2000 {
        t.h()?; // viewport entity header control
    }
    for _ in 0..3 {
        t.h()?; // dictionaries
    }
    shorts(t, 2)?; // TSTACKALIGN TSTACKSIZE
    t.tv()?;
    t.tv()?;
    for _ in 0..3 {
        t.h()?; // layouts, plot settings, plot styles
    }
    if v >= R2004 {
        t.h()?;
        t.h()?;
    }
    if v >= R2007 {
        t.h()?;
    }
    if v >= R2013 {
        t.h()?;
    }
    t.m.bl()?; // flags
    o.insunits = Some(t.m.bs()?);
    Ok(())
}

/// INSBASE .. UCS block of one space; fills `o` with the extents and limits.
fn space(t: &mut Tri, v: Version, o: &mut HeaderInfo) -> R<()> {
    o.insbase = Some(t.m.p3()?);
    o.extmin = Some(t.m.p3()?);
    o.extmax = Some(t.m.p3()?);
    o.limmin = Some(t.m.p2r()?);
    o.limmax = Some(t.m.p2r()?);
    t.m.bd()?; // ELEVATION
    for _ in 0..3 {
        t.m.p3()?; // UCSORG UCSXDIR UCSYDIR
    }
    t.h()?; // UCSNAME
    if v >= Version::R2000 {
        t.h()?;
        t.m.bs()?;
        t.h()?;
        for _ in 0..6 {
            t.m.p3()?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct Class {
    pub num: u16,
    pub dxf: String,
    /// Item class id 0x1F2: instances are graphic entities.
    pub entity: bool,
}

pub(crate) fn classes(d: &[u8], ver: Version, cp: u16, rep: &mut DwgReport) -> Vec<Class> {
    let mut out = vec![];
    if d.len() < 24 {
        return out;
    }
    if let Err(e) = class_seq(d, ver, cp, &mut out) {
        rep.warn(format!("classes partly read ({} read): {}", out.len(), e.0));
    }
    out
}

fn class_seq(d: &[u8], ver: Version, cp: u16, out: &mut Vec<Class>) -> R<()> {
    let mut t = streams(d, ver, codepage_index(cp))?;
    t.h_inline = false;
    if ver <= Version::R2000 {
        while t.m.left() >= 32 {
            let num = t.m.bs()?;
            t.m.bs()?;
            t.tv()?;
            t.tv()?;
            let dxf = t.tv()?;
            t.m.b()?;
            let item = t.m.bs()?;
            out.push(Class {
                num,
                dxf,
                entity: item == 0x1F2,
            });
        }
        return Ok(());
    }
    let max = t.m.bs()?;
    t.m.rc()?;
    t.m.rc()?;
    t.m.b()?;
    while t.m.left() > 0 && out.len() < 8192 {
        let num = t.m.bs()?;
        t.m.bs()?;
        let dxf = if ver == Version::R2004 {
            t.tv()?;
            t.tv()?;
            t.tv()?
        } else {
            String::new()
        };
        t.m.b()?;
        let item = t.m.bs()?;
        for _ in 0..5 {
            t.m.bl()?;
        }
        out.push(Class {
            num,
            dxf,
            entity: item == 0x1F2,
        });
        if num >= max {
            break;
        }
    }
    if ver >= Version::R2007 {
        if let Some(s) = t.s.as_mut() {
            for c in out.iter_mut() {
                s.tu()?;
                s.tu()?;
                c.dxf = s.tu()?;
            }
        }
    }
    Ok(())
}
