// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Object map, object framing, common entity/object data, table records and entity bodies
//! (spec: docs/specs/DWG.md §6–8). Decoders return `Err` on any inconsistency; the caller
//! counts the object as failed and continues.

use super::annot;
use super::bits::{crc16, mc, ms, umc, Bits};
use super::header::{split_strings, Tri};
use super::{err, DwgReport, Version, R};

pub(crate) type P3 = [f64; 3];
pub(crate) type P2 = [f64; 2];

/// Handle map: (handle, offset into the object data), in stored order.
pub(crate) fn handle_map(d: &[u8], rep: &mut DwgReport) -> Vec<(u64, usize)> {
    let mut out = vec![];
    let mut pos = 0usize;
    while pos + 2 <= d.len() {
        let size = u16::from_be_bytes([d[pos], d[pos + 1]]) as usize;
        if size <= 2 {
            break;
        }
        let end = pos + size;
        if end + 2 > d.len() {
            rep.warn("object map chunk truncated");
            break;
        }
        let stored = u16::from_be_bytes([d[end], d[end + 1]]);
        if crc16(0xC0C1, &d[pos..end]) != stored {
            rep.warn("object map chunk CRC mismatch");
        }
        let (mut h, mut off) = (0u64, 0i64);
        let mut p = pos + 2;
        while p < end {
            let (Ok(dh), Ok(doff)) = (umc(&d[..end], &mut p), mc(&d[..end], &mut p)) else {
                rep.warn("object map entry truncated");
                break;
            };
            h = h.wrapping_add(dh);
            off = off.wrapping_add(doff);
            if off >= 0 {
                out.push((h, off as usize));
            }
        }
        pos = end + 2;
    }
    out
}

/// One framed object: type, own handle and the reader positioned after the extended data.
pub(crate) struct Frame<'a> {
    pub handle: u64,
    pub ty: u16,
    payload: &'a [u8],
    r: Bits<'a>,
    /// End of the data area in bits (start of the handle stream), when stored up front.
    data_end: Option<usize>,
    pub crc_ok: bool,
}

pub(crate) fn frame(objs: &[u8], off: usize, ver: Version) -> R<Frame<'_>> {
    let mut pos = off;
    let size = ms(objs, &mut pos)? as usize;
    let hbits = if ver >= Version::R2010 {
        umc(objs, &mut pos)? as usize
    } else {
        0
    };
    let end = pos
        .checked_add(size)
        .filter(|&e| size > 0 && e <= objs.len())
        .ok_or_else(|| err("object frame outside data"))?;
    let payload = &objs[pos..end];
    let crc_ok = objs
        .get(end..end + 2)
        .map(|c| crc16(0xC0C1, &objs[off..end]) == u16::from_le_bytes([c[0], c[1]]))
        .unwrap_or(false);
    let mut r = Bits::new(payload);
    let ty = if ver >= Version::R2010 {
        match r.bb()? {
            0 => r.rc()? as u16,
            1 => r.rc()? as u16 + 0x1F0,
            _ => r.rs()?,
        }
    } else {
        r.bs()?
    };
    let mut data_end = None;
    if ver >= Version::R2010 {
        data_end = Some(
            size.saturating_mul(8)
                .checked_sub(hbits)
                .ok_or_else(|| err("handle stream size"))?,
        );
    } else if ver >= Version::R2000 {
        data_end = Some(r.rl()? as usize);
    }
    let (_, handle) = r.h()?;
    loop {
        let n = r.bs()? as usize;
        if n == 0 {
            break;
        }
        r.h()?;
        r.skip(n.saturating_mul(8))?;
    }
    Ok(Frame {
        handle,
        ty,
        payload,
        r,
        data_end,
        crc_ok,
    })
}

impl<'a> Frame<'a> {
    /// Data / string / handle streams once the data size is known.
    fn tri(&self, r: Bits<'a>, data_end: usize, ver: Version, cp: Option<usize>) -> R<Tri<'a>> {
        let max = self.payload.len() * 8;
        if data_end > max || data_end < r.pos {
            return Err(err("object data size outside the record"));
        }
        let (m_end, s) = if ver >= Version::R2007 {
            split_strings(self.payload, r.pos, data_end)
        } else {
            (data_end, None)
        };
        let mut m = r;
        m.end = m_end.max(m.pos);
        Ok(Tri {
            m,
            s,
            h: Bits::view(self.payload, data_end, max),
            h_inline: false,
            unicode: ver >= Version::R2007,
            cp,
        })
    }
}

/// Linetype reference of an entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Lt {
    ByLayer,
    ByBlock,
    Continuous,
    Handle(u64),
}

#[derive(Debug, Clone)]
pub(crate) struct ECom {
    /// 0 = owned (see `owner`), 1 = paper space, 2 = model space.
    pub mode: u8,
    pub owner: u64,
    pub layer: u64,
    pub lt: Lt,
    pub color: i32,
    pub true_color: Option<i32>,
    pub lw_index: Option<u8>,
    pub ltscale: f64,
    pub invisible: bool,
    /// Next sibling (R13–R2000 entity chain); 0 when not stored.
    pub next: u64,
}

pub(crate) fn open_entity<'a>(
    f: &Frame<'a>,
    ver: Version,
    cp: Option<usize>,
) -> R<(Tri<'a>, ECom)> {
    use Version::*;
    let mut r = f.r.clone();
    if r.b()? {
        let n = if ver >= R2010 {
            r.bll()?
        } else {
            r.rl()? as u64
        } as usize;
        r.skip(n.saturating_mul(8))?;
    }
    let data_end = match f.data_end {
        Some(d) => d,
        None => r.rl()? as usize,
    };
    let mode = r.bb()?;
    let reactors = r.bl()? as usize;
    let xdic_missing = ver >= R2004 && r.b()?;
    if ver >= R2013 {
        r.b()?;
    }
    let bylayer_lt = ver <= R14 && r.b()?;
    let nolinks = ver <= R2000 && r.b()?;
    let color;
    let (mut true_color, mut book) = (None, false);
    if ver <= R2000 {
        color = r.bs()? as i16 as i32;
    } else {
        let w = r.bs()?;
        let flags = w >> 8;
        color = (w & 0x1FF) as i32;
        // 0x40 = colour-book reference (handle in the handle stream, no colour value here).
        if flags & 0x80 != 0 && flags & 0x40 == 0 {
            let rgb = r.bl()?;
            if rgb >> 24 == 0xC2 {
                true_color = Some((rgb & 0xFF_FFFF) as i32);
            }
        }
        book = flags & 0x40 != 0;
        if flags & 0x20 != 0 {
            r.bl()?;
        }
    }
    let ltscale = r.bd()?;
    let (mut ltf, mut matf, mut psf) = (0u8, 0u8, 0u8);
    if ver >= R2000 {
        ltf = r.bb()?;
        if ver >= R2007 {
            matf = r.bb()?;
        }
        psf = r.bb()?;
        if ver >= R2007 {
            r.rc()?;
        }
    }
    let mut vs = [false; 3];
    if ver >= R2010 {
        for v in &mut vs {
            *v = r.b()?;
        }
    }
    let invisible = r.bs()? & 1 != 0;
    let lw_index = if ver >= R2000 { Some(r.rc()?) } else { None };
    let mut t = f.tri(r, data_end, ver, cp)?;
    let own = f.handle;
    let h = &mut t.h;
    let owner = if mode == 0 { h.href(own)? } else { 0 };
    if reactors > h.left() / 8 {
        return Err(err("reactor count exceeds record"));
    }
    for _ in 0..reactors {
        h.h()?;
    }
    if !xdic_missing {
        h.h()?;
    }
    let mut layer = 0;
    let mut lt = Lt::ByLayer;
    if ver <= R14 {
        layer = h.href(own)?;
        if !bylayer_lt {
            lt = Lt::Handle(h.href(own)?);
        }
    }
    let mut next = 0;
    if ver <= R2000 {
        if nolinks {
            next = own.wrapping_add(1);
        } else {
            h.href(own)?; // previous
            next = h.href(own)?;
        }
    }
    if ver >= R2004 && book {
        h.h()?;
    }
    if ver >= R2000 {
        layer = h.href(own)?;
        lt = match ltf {
            0 => Lt::ByLayer,
            1 => Lt::ByBlock,
            2 => Lt::Continuous,
            _ => Lt::Handle(h.href(own)?),
        };
    }
    if ver >= R2007 && matf == 3 {
        h.h()?;
    }
    if ver >= R2000 && psf == 3 {
        h.h()?;
    }
    for v in vs {
        if v {
            h.h()?;
        }
    }
    Ok((
        t,
        ECom {
            mode,
            owner,
            layer,
            lt,
            color,
            true_color,
            lw_index,
            ltscale,
            invisible,
            next,
        },
    ))
}

/// Non-entity object: streams positioned after the common data, handle stream after
/// owner, reactors and extension dictionary.
pub(crate) fn open_object<'a>(f: &Frame<'a>, ver: Version, cp: Option<usize>) -> R<Tri<'a>> {
    let mut r = f.r.clone();
    let data_end = match f.data_end {
        Some(d) => d,
        None => r.rl()? as usize,
    };
    let reactors = r.bl()? as usize;
    let xdic_missing = ver >= Version::R2004 && r.b()?;
    if ver >= Version::R2013 {
        r.b()?;
    }
    let mut t = f.tri(r, data_end, ver, cp)?;
    t.h.h()?;
    if reactors > t.h.left() / 8 {
        return Err(err("reactor count exceeds record"));
    }
    for _ in 0..reactors {
        t.h.h()?;
    }
    if !xdic_missing {
        t.h.h()?;
    }
    Ok(t)
}

// ---------------------------------------------------------------- table records

/// Name, 64-flag, xref index (before R2007), xref-dependent bit.
pub(crate) fn table_name(t: &mut Tri, ver: Version) -> R<String> {
    let name = t.tv()?;
    t.m.b()?;
    if ver < Version::R2007 {
        t.m.bs()?;
    }
    t.m.b()?;
    Ok(name)
}

#[derive(Debug, Clone)]
pub(crate) struct LayerRec {
    pub name: String,
    /// DXF code 70 bits: 1 frozen, 2 frozen in new viewports, 4 locked.
    pub flags: i32,
    pub off: bool,
    pub color: i32,
    pub true_color: Option<i32>,
    pub ltype: u64,
    pub lw_index: Option<u8>,
}

pub(crate) fn layer(t: &mut Tri, ver: Version, own: u64) -> R<LayerRec> {
    let name = table_name(t, ver)?;
    let (flags, off, lw_index);
    if ver <= Version::R14 {
        let frozen = t.m.b()?;
        // The second bit does not track visibility in R13/R14 files (it reads clear on
        // visible layers); hidden layers are the ones with a negative colour.
        let _on_bit = t.m.b()?;
        let fnew = t.m.b()?;
        let locked = t.m.b()?;
        flags = frozen as i32 | (fnew as i32) << 1 | (locked as i32) << 2;
        off = false;
        lw_index = None;
    } else {
        let f = t.m.bs()?;
        flags = (f & 1) as i32 | ((f & 4 != 0) as i32) << 1 | ((f & 8 != 0) as i32) << 2;
        off = f & 2 != 0;
        lw_index = Some(((f >> 5) & 31) as u8);
    }
    let (color, true_color) = t.cmc(ver)?;
    t.h.h()?; // xref block
    if ver >= Version::R2000 {
        t.h.h()?; // plot style
    }
    if ver >= Version::R2007 {
        t.h.h()?; // material
    }
    let ltype = t.h.href(own)?;
    Ok(LayerRec {
        name,
        flags,
        off: off || color < 0,
        color: color.abs(),
        true_color,
        ltype,
        lw_index,
    })
}

#[derive(Debug, Clone)]
pub(crate) struct LtypeRec {
    pub name: String,
    pub desc: String,
    pub length: f64,
    pub dashes: Vec<f64>,
}

pub(crate) fn ltype(t: &mut Tri, ver: Version) -> R<LtypeRec> {
    let name = table_name(t, ver)?;
    let desc = t.tv()?;
    let length = t.m.bd()?;
    t.m.rc()?;
    let n = t.m.rc()?;
    let mut dashes = vec![];
    for _ in 0..n {
        dashes.push(t.m.bd()?);
        t.m.bs()?;
        t.m.rd()?;
        t.m.rd()?;
        t.m.bd()?;
        t.m.bd()?;
        t.m.bs()?;
    }
    Ok(LtypeRec {
        name,
        desc,
        length,
        dashes,
    })
}

#[derive(Debug, Clone)]
pub(crate) struct StyleRec {
    pub name: String,
    pub flags: i32,
    pub height: f64,
    pub width: f64,
    pub oblique: f64,
    pub font: String,
    pub big_font: String,
}

pub(crate) fn style(t: &mut Tri, ver: Version) -> R<StyleRec> {
    let name = table_name(t, ver)?;
    let shape = t.m.b()?;
    let vertical = t.m.b()?;
    let height = t.m.bd()?;
    let width = t.m.bd()?;
    let oblique = t.m.bd()?;
    t.m.rc()?;
    t.m.bd()?;
    let font = t.tv()?;
    let big_font = t.tv()?;
    Ok(StyleRec {
        name,
        flags: shape as i32 | (vertical as i32) << 2,
        height,
        width,
        oblique,
        font,
        big_font,
    })
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BlockRec {
    pub name: String,
    /// DXF code 70 bits: 1 anonymous, 2 has attributes, 4 xref, 8 overlaid.
    pub flags: i32,
    pub base: P3,
    pub block_ent: u64,
    /// Owned entities in order (R2004+).
    pub owned: Vec<u64>,
    /// First and last entity (R13–R2000), walked through the entities' next links.
    pub chain: Option<(u64, u64)>,
}

pub(crate) fn block_rec(t: &mut Tri, ver: Version, own: u64) -> R<BlockRec> {
    let name = table_name(t, ver)?;
    let mut flags = 0;
    for bit in [1, 2, 4, 8] {
        if t.m.b()? {
            flags |= bit;
        }
    }
    if ver >= Version::R2000 {
        t.m.b()?;
    }
    let count = if ver >= Version::R2004 {
        t.m.bl()? as usize
    } else {
        0
    };
    let base = t.m.p3()?;
    t.h.h()?; // xref / null
    let block_ent = t.h.href(own)?;
    let mut chain = None;
    if ver <= Version::R2000 {
        let first = t.h.href(own)?;
        chain = Some((first, t.h.href(own)?));
    }
    if count > t.h.left() / 8 {
        return Err(err("owned object count exceeds record"));
    }
    let mut owned = Vec::with_capacity(count.min(MAX_PREALLOC));
    for _ in 0..count {
        owned.push(t.h.href(own)?);
    }
    Ok(BlockRec {
        name,
        flags,
        base,
        block_ent,
        owned,
        chain,
    })
}

// ---------------------------------------------------------------- entities

#[derive(Debug, Clone, Default)]
pub(crate) struct TextD {
    /// Insertion x, y and elevation.
    pub ins: P3,
    pub align: Option<P2>,
    pub ext: P3,
    pub thickness: f64,
    pub oblique: f64,
    pub rotation: f64,
    pub height: f64,
    pub width: f64,
    pub text: String,
    pub generation: u16,
    pub halign: u16,
    pub valign: u16,
    pub style: u64,
}

#[derive(Debug, Clone)]
pub(crate) enum Edge {
    Line(P2, P2),
    Arc {
        c: P2,
        r: f64,
        a0: f64,
        a1: f64,
        ccw: bool,
    },
    Ellipse {
        c: P2,
        major: P2,
        ratio: f64,
        a0: f64,
        a1: f64,
        ccw: bool,
    },
    Spline {
        degree: u32,
        rational: bool,
        periodic: bool,
        knots: Vec<f64>,
        ctrl: Vec<(P2, f64)>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum Path {
    Edges(u32, Vec<Edge>),
    Poly {
        flags: u32,
        closed: bool,
        pts: Vec<(P2, f64)>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct DimD {
    /// DXF code 70 low bits: 0 linear/rotated, 1 aligned, 2 angular, 3 diameter,
    /// 4 radius, 5 three-point angular, 6 ordinate.
    pub kind: i32,
    pub ext: P3,
    pub text_mid: P2,
    pub elevation: f64,
    pub text: String,
    pub text_rot: f64,
    pub attach: u16,
    pub measure: f64,
    #[allow(dead_code)] // read to keep the stream aligned; not exported
    pub clone: P2,
    /// DXF 10, 13, 14, 15, 16 points (absent ones are None).
    pub p10: P3,
    pub p13: Option<P3>,
    pub p14: Option<P3>,
    pub p15: Option<P3>,
    pub p16: Option<P2>,
    pub rot: f64,
    pub ext_rot: f64,
    pub leader: f64,
    pub ordinate_x: bool,
    pub style: u64,
    pub block: u64,
}

#[derive(Debug, Clone)]
pub(crate) enum Kind {
    Line {
        a: P3,
        b: P3,
        th: f64,
        ext: P3,
    },
    Circle {
        c: P3,
        r: f64,
        th: f64,
        ext: P3,
    },
    Arc {
        c: P3,
        r: f64,
        th: f64,
        ext: P3,
        a0: f64,
        a1: f64,
    },
    Point {
        p: P3,
        th: f64,
        ext: P3,
    },
    Text(TextD),
    Attrib {
        t: TextD,
        tag: String,
        flags: u8,
    },
    AttDef {
        t: TextD,
        tag: String,
        prompt: String,
        flags: u8,
    },
    MText {
        ins: P3,
        ext: P3,
        xdir: P3,
        width: f64,
        height: f64,
        attach: u16,
        text: String,
        style: u64,
    },
    Insert {
        ins: P3,
        scale: P3,
        rot: f64,
        ext: P3,
        block: u64,
        attribs: Owned,
        grid: Option<(u16, u16, f64, f64)>,
    },
    Block,
    EndBlk,
    SeqEnd,
    Vertex {
        flags: u8,
        p: P3,
        w0: f64,
        w1: f64,
        bulge: f64,
    },
    FaceRec([i16; 4]),
    Poly2d {
        flags: u16,
        w0: f64,
        w1: f64,
        th: f64,
        elev: f64,
        ext: P3,
        owned: Owned,
    },
    /// 3D polyline (flags 8), polyface mesh (64) or polygon mesh (16): DXF code 70 and counts.
    PolyN {
        flags: i32,
        m: u16,
        n: u16,
        owned: Owned,
    },
    Ellipse {
        c: P3,
        major: P3,
        ext: P3,
        ratio: f64,
        a0: f64,
        a1: f64,
    },
    Spline {
        flags: i32,
        degree: u32,
        knots: Vec<f64>,
        ctrl: Vec<P3>,
        weights: Vec<f64>,
        fit: Vec<P3>,
        tangents: Option<(P3, P3)>,
    },
    Solid {
        trace: bool,
        pts: [P3; 4],
        th: f64,
        ext: P3,
    },
    Face3d {
        pts: [P3; 4],
        flags: u16,
    },
    LwPoly {
        flags: u16,
        const_w: f64,
        elev: f64,
        th: f64,
        ext: P3,
        pts: Vec<P2>,
        bulges: Vec<f64>,
        widths: Vec<(f64, f64)>,
    },
    Dim(Box<DimD>),
    Hatch {
        elev: f64,
        ext: P3,
        name: String,
        solid: bool,
        assoc: bool,
        paths: Vec<Path>,
        style: u16,
        ptype: u16,
        angle: f64,
        scale: f64,
        double: bool,
        lines: Vec<PatLine>,
        seeds: Vec<P2>,
    },
    Viewport {
        c: P3,
        w: f64,
        h: f64,
    },
    Ray {
        xline: bool,
        p: P3,
        v: P3,
    },
    Leader(Box<annot::LeaderD>),
    MLeader(Box<annot::MLeaderD>),
    Image {
        wipeout: bool,
        d: Box<annot::ImageD>,
    },
    /// REGION / 3DSOLID / BODY: kept opaque, with the modeler text when stored inline.
    Acis {
        name: &'static str,
        sat: Option<String>,
    },
    /// Class entity derived from INSERT (ACIDBLOCKREFERENCE): the block-reference part only.
    BlockRef {
        ins: P3,
        scale: P3,
        rot: f64,
        ext: P3,
        block: u64,
    },
    /// ACAD_TABLE: drawn through its anonymous block (lines and texts), scale 1.
    Table {
        ins: P3,
        rot: f64,
        ext: P3,
        block: u64,
    },
    /// OLE2FRAME: the frame is kept; the embedded document is not carried.
    Ole {
        version: u16,
    },
}

/// Hatch pattern definition line (angle in degrees), as stored: already scaled and rotated.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PatLine {
    pub angle: f64,
    pub base: P2,
    pub offset: P2,
    pub dashes: Vec<f64>,
}

/// DICTIONARY (type 42): entry names with their item handles, in stored order.
pub(crate) fn dictionary(t: &mut Tri, ver: Version, own: u64) -> R<Vec<(String, u64)>> {
    let n = t.m.bl()? as usize;
    if ver == Version::R14 {
        t.m.rc()?;
    } else if ver >= Version::R2000 {
        t.m.bs()?;
        t.m.rc()?;
    }
    if n > t.h.left() / 8 {
        return Err(err("dictionary entry count exceeds record"));
    }
    let mut names = vec![];
    for _ in 0..n {
        names.push(t.tv()?);
    }
    names.into_iter().map(|s| Ok((s, t.h.href(own)?))).collect()
}

/// Children of a POLYLINE / INSERT.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Owned {
    /// Explicit handle list (R2004+); empty = use the children found by owner.
    List(Vec<u64>),
    /// First and last child (R13–R2000), linked through each child's next handle.
    Chain(u64, u64),
}

/// Ceiling for pre-allocation from a stored count; vectors grow past it only as items decode.
const MAX_PREALLOC: usize = 1 << 16;

/// Pre-allocation for `n` items of at least `min_bits` stored bits each.
fn room(n: usize, left_bits: usize, min_bits: usize) -> usize {
    n.min(left_bits / min_bits.max(1)).min(MAX_PREALLOC)
}

/// BL item count, bounded by the bits left in the data stream.
fn cnt(t: &mut Tri) -> R<usize> {
    let n = t.m.bl()? as usize;
    if n > t.m.left() {
        return Err(err("item count exceeds record"));
    }
    Ok(n)
}

fn text_body(t: &mut Tri, v: Version) -> R<TextD> {
    let mut d = TextD {
        width: 1.0,
        ..Default::default()
    };
    if v <= Version::R14 {
        let elev = t.m.bd()?;
        let ins = t.m.p2r()?;
        d.ins = [ins[0], ins[1], elev];
        d.align = Some(t.m.p2r()?);
        d.ext = t.m.p3()?;
        d.thickness = t.m.bd()?;
        d.oblique = t.m.bd()?;
        d.rotation = t.m.bd()?;
        d.height = t.m.bd()?;
        d.width = t.m.bd()?;
        d.text = t.tv()?;
        d.generation = t.m.bs()?;
        d.halign = t.m.bs()?;
        d.valign = t.m.bs()?;
        return Ok(d);
    }
    let df = t.m.rc()?;
    let elev = if df & 1 != 0 { 0.0 } else { t.m.rd()? };
    let (x, y) = (t.m.rd_raw()?, t.m.rd_raw()?);
    // Non-finite stored insertion: written as 0,0 by the reference exports; an alignment
    // point (DD against it) would be undefined, so that combination is refused.
    let ins = if x.is_finite() && y.is_finite() {
        [x, y]
    } else if df & 2 != 0 {
        [0.0, 0.0]
    } else {
        return Err(err("TEXT alignment after non-finite insertion"));
    };
    d.ins = [ins[0], ins[1], elev];
    d.align = if df & 2 != 0 {
        None
    } else {
        Some(t.m.p2d(ins)?)
    };
    d.ext = t.m.be(true)?;
    d.thickness = t.m.bt(true)?;
    if df & 4 == 0 {
        d.oblique = t.m.rd()?;
    }
    if df & 8 == 0 {
        d.rotation = t.m.rd()?;
    }
    d.height = t.m.rd()?;
    if df & 0x10 == 0 {
        d.width = t.m.rd()?;
    }
    d.text = t.tv()?;
    if df & 0x20 == 0 {
        d.generation = t.m.bs()?;
    }
    if df & 0x40 == 0 {
        d.halign = t.m.bs()?;
    }
    if df & 0x80 == 0 {
        d.valign = t.m.bs()?;
    }
    Ok(d)
}

/// Attribute tail: (tag, flags); `None` for multi-line attributes (R2018+), which keep only the text.
fn attrib_tail(t: &mut Tri, v: Version) -> R<Option<(String, u8)>> {
    if v >= Version::R2010 {
        t.m.rc()?;
    }
    if v >= Version::R2018 && t.m.rc()? > 1 {
        return Ok(None);
    }
    let tag = t.tv()?;
    t.m.bs()?;
    let flags = t.m.rc()?;
    if v >= Version::R2007 {
        t.m.b()?;
    }
    Ok(Some((tag, flags)))
}

/// Child handles of a POLYLINE/INSERT: first/last before R2004, an explicit list after.
fn owned_list(t: &mut Tri, v: Version, own: u64, count: usize) -> R<Owned> {
    if v <= Version::R2000 {
        let first = t.h.href(own)?;
        return Ok(Owned::Chain(first, t.h.href(own)?));
    }
    if count > t.h.left() / 8 {
        return Err(err("owned count exceeds record"));
    }
    (0..count)
        .map(|_| t.h.href(own))
        .collect::<R<Vec<_>>>()
        .map(Owned::List)
}

fn read_spline(t: &mut Tri, v: Version) -> R<Kind> {
    let mut scen = t.m.bl()?;
    if v >= Version::R2013 {
        let f1 = t.m.bl()?;
        let kp = t.m.bl()?;
        if f1 & 1 != 0 {
            scen = 2;
        } else if kp == 15 {
            scen = 1;
        }
    }
    let degree = t.m.bl()?;
    let (mut flags, mut knots, mut ctrl, mut weights, mut fit, mut tangents) =
        (0, vec![], vec![], vec![], vec![], None);
    if scen == 2 {
        t.m.bd()?;
        let t0 = t.m.p3()?;
        let t1 = t.m.p3()?;
        tangents = Some((t0, t1));
        let n = cnt(t)?;
        for _ in 0..n {
            fit.push(t.m.p3()?);
        }
        flags |= 8;
    } else {
        let rational = t.m.b()?;
        let closed = t.m.b()?;
        let periodic = t.m.b()?;
        t.m.bd()?;
        t.m.bd()?;
        let nk = cnt(t)?;
        let nc = cnt(t)?;
        let weighted = t.m.b()?;
        flags |= closed as i32 | (periodic as i32) << 1 | (rational as i32) << 2;
        for _ in 0..nk {
            knots.push(t.m.bd()?);
        }
        for _ in 0..nc {
            ctrl.push(t.m.p3()?);
            if weighted {
                weights.push(t.m.bd()?);
            }
        }
    }
    Ok(Kind::Spline {
        flags,
        degree,
        knots,
        ctrl,
        weights,
        fit,
        tangents,
    })
}

fn read_hatch(t: &mut Tri, v: Version) -> R<Kind> {
    if v >= Version::R2004 {
        t.m.bl()?;
        t.m.bl()?;
        t.m.bd()?;
        t.m.bd()?;
        t.m.bl()?;
        t.m.bd()?;
        let n = cnt(t)?;
        for _ in 0..n {
            t.m.bd()?;
            t.cmc(v)?;
        }
        t.tv()?;
    }
    let elev = t.m.bd()?;
    let ext = t.m.p3()?;
    let name = t.tv()?;
    let solid = t.m.b()?;
    let assoc = t.m.b()?;
    let np = cnt(t)?;
    let mut paths = Vec::with_capacity(room(np, t.m.left(), 6));
    let mut pixel = false;
    for _ in 0..np {
        let flag = t.m.bl()?;
        pixel |= flag & 4 != 0;
        if flag & 2 == 0 {
            let ns = cnt(t)?;
            let mut edges = Vec::with_capacity(room(ns, t.m.left(), 8));
            for _ in 0..ns {
                edges.push(match t.m.rc()? {
                    1 => Edge::Line(t.m.p2r()?, t.m.p2r()?),
                    2 => Edge::Arc {
                        c: t.m.p2r()?,
                        r: t.m.bd()?,
                        a0: t.m.bd()?,
                        a1: t.m.bd()?,
                        ccw: t.m.b()?,
                    },
                    3 => Edge::Ellipse {
                        c: t.m.p2r()?,
                        major: t.m.p2r()?,
                        ratio: t.m.bd()?,
                        a0: t.m.bd()?,
                        a1: t.m.bd()?,
                        ccw: t.m.b()?,
                    },
                    4 => {
                        let degree = t.m.bl()?;
                        let rational = t.m.b()?;
                        let periodic = t.m.b()?;
                        let nk = cnt(t)?;
                        let nc = cnt(t)?;
                        let knots = (0..nk).map(|_| t.m.bd()).collect::<R<Vec<_>>>()?;
                        let mut ctrl = Vec::with_capacity(room(nc, t.m.left(), 64));
                        for _ in 0..nc {
                            let p = t.m.p2r()?;
                            let w = if rational { t.m.bd()? } else { 1.0 };
                            ctrl.push((p, w));
                        }
                        if v >= Version::R2010 {
                            let nf = cnt(t)?;
                            if nf > 0 {
                                for _ in 0..nf {
                                    t.m.p2r()?;
                                }
                                t.m.p2r()?;
                                t.m.p2r()?;
                            }
                        }
                        Edge::Spline {
                            degree,
                            rational,
                            periodic,
                            knots,
                            ctrl,
                        }
                    }
                    k => return Err(err(format!("hatch edge type {k}"))),
                });
            }
            paths.push(Path::Edges(flag, edges));
        } else {
            let bulges = t.m.b()?;
            let closed = t.m.b()?;
            let n = cnt(t)?;
            let mut pts = Vec::with_capacity(room(n, t.m.left(), 128));
            for _ in 0..n {
                let p = t.m.p2r()?;
                let b = if bulges { t.m.bd()? } else { 0.0 };
                pts.push((p, b));
            }
            paths.push(Path::Poly {
                flags: flag,
                closed,
                pts,
            });
        }
        let nb = cnt(t)?;
        for _ in 0..nb {
            t.h.h()?;
        }
    }
    let style = t.m.bs()?;
    let ptype = t.m.bs()?;
    let (mut angle, mut scale, mut double, mut lines) = (0.0, 1.0, false, vec![]);
    if !solid {
        angle = t.m.bd()?;
        scale = t.m.bd()?;
        double = t.m.b()?;
        let nl = t.m.bs()?;
        for _ in 0..nl {
            let a = t.m.bd()?;
            let base = [t.m.bd()?, t.m.bd()?];
            let offset = [t.m.bd()?, t.m.bd()?];
            let nd = t.m.bs()?;
            let dashes = (0..nd).map(|_| t.m.bd()).collect::<R<Vec<_>>>()?;
            lines.push(PatLine {
                angle: a.to_degrees(),
                base,
                offset,
                dashes,
            });
        }
    }
    if pixel {
        t.m.bd()?;
    }
    let ns = cnt(t)?;
    let seeds = (0..ns).map(|_| t.m.p2r()).collect::<R<Vec<_>>>()?;
    Ok(Kind::Hatch {
        elev,
        ext,
        name,
        solid,
        assoc,
        paths,
        style,
        ptype,
        angle: angle.to_degrees(),
        scale,
        double,
        lines,
        seeds,
    })
}

fn read_dim(t: &mut Tri, v: Version, ty: u16, own: u64) -> R<Kind> {
    if v >= Version::R2010 {
        t.m.rc()?;
    }
    let ext = t.m.p3()?;
    let text_mid = t.m.p2r()?;
    let elevation = t.m.bd()?;
    let flags1 = t.m.rc()?;
    let text = t.tv()?;
    let text_rot = t.m.bd()?;
    t.m.bd()?; // horizontal direction
    t.m.p3()?; // insertion scale
    t.m.bd()?; // insertion rotation
    let (mut attach, mut measure) = (5u16, 0.0);
    if v >= Version::R2000 {
        attach = t.m.bs()?;
        t.m.bs()?;
        t.m.bd()?;
        measure = t.m.bd()?;
    }
    if v >= Version::R2007 {
        t.m.b()?;
        t.m.b()?;
        t.m.b()?;
    }
    let clone = t.m.p2r()?;
    let mut d = DimD {
        kind: 0,
        ext,
        text_mid,
        elevation,
        text,
        text_rot: text_rot.to_degrees(),
        attach,
        measure,
        clone,
        p10: [0.0; 3],
        p13: None,
        p14: None,
        p15: None,
        p16: None,
        rot: 0.0,
        ext_rot: 0.0,
        leader: 0.0,
        ordinate_x: false,
        style: 0,
        block: 0,
    };
    match ty {
        20 => {
            d.kind = 6;
            d.p10 = t.m.p3()?;
            d.p13 = Some(t.m.p3()?);
            d.p14 = Some(t.m.p3()?);
            d.ordinate_x = t.m.rc()? & 1 != 0;
        }
        21 | 22 => {
            d.kind = if ty == 21 { 0 } else { 1 };
            d.p13 = Some(t.m.p3()?);
            d.p14 = Some(t.m.p3()?);
            d.p10 = t.m.p3()?;
            d.ext_rot = t.m.bd()?.to_degrees();
            if ty == 21 {
                d.rot = t.m.bd()?.to_degrees();
            }
        }
        23 => {
            d.kind = 5;
            d.p10 = t.m.p3()?;
            d.p13 = Some(t.m.p3()?);
            d.p14 = Some(t.m.p3()?);
            d.p15 = Some(t.m.p3()?);
        }
        24 => {
            d.kind = 2;
            d.p16 = Some(t.m.p2r()?);
            d.p13 = Some(t.m.p3()?);
            d.p14 = Some(t.m.p3()?);
            d.p15 = Some(t.m.p3()?);
            d.p10 = t.m.p3()?;
        }
        _ => {
            d.kind = if ty == 25 { 4 } else { 3 };
            if ty == 25 {
                d.p10 = t.m.p3()?;
                d.p15 = Some(t.m.p3()?);
            } else {
                d.p15 = Some(t.m.p3()?);
                d.p10 = t.m.p3()?;
            }
            d.leader = t.m.bd()?;
        }
    }
    let _ = flags1;
    d.style = t.h.href(own)?;
    d.block = t.h.href(own)?;
    Ok(Kind::Dim(Box::new(d)))
}

fn lwpoly(t: &mut Tri, v: Version) -> R<Kind> {
    let flag = t.m.bs()?;
    let const_w = if flag & 4 != 0 { t.m.bd()? } else { 0.0 };
    let elev = if flag & 8 != 0 { t.m.bd()? } else { 0.0 };
    let th = if flag & 2 != 0 { t.m.bd()? } else { 0.0 };
    let ext = if flag & 1 != 0 {
        t.m.p3()?
    } else {
        [0.0, 0.0, 1.0]
    };
    let n = cnt(t)?;
    let nb = if flag & 16 != 0 { cnt(t)? } else { 0 };
    let nid = if v >= Version::R2010 && flag & 1024 != 0 {
        cnt(t)?
    } else {
        0
    };
    let nw = if flag & 32 != 0 { cnt(t)? } else { 0 };
    let mut pts: Vec<P2> = Vec::with_capacity(room(n, t.m.left(), 4));
    for i in 0..n {
        let p = if v <= Version::R14 || i == 0 {
            t.m.p2r()?
        } else {
            t.m.p2d(pts[i - 1])?
        };
        pts.push(p);
    }
    let bulges = (0..nb).map(|_| t.m.bd()).collect::<R<Vec<_>>>()?;
    for _ in 0..nid {
        t.m.bl()?;
    }
    let widths = (0..nw)
        .map(|_| Ok((t.m.bd()?, t.m.bd()?)))
        .collect::<R<Vec<_>>>()?;
    Ok(Kind::LwPoly {
        flags: flag,
        const_w,
        elev,
        th,
        ext,
        pts,
        bulges,
        widths,
    })
}

/// Entity body for a fixed type code or a class name; `Ok(None)` = kind not converted.
pub(crate) fn entity(
    t: &mut Tri,
    v: Version,
    ty: u16,
    class: Option<&str>,
    own: u64,
) -> R<Option<Kind>> {
    let r2000 = v >= Version::R2000;
    Ok(Some(match ty {
        1 => {
            let mut d = text_body(t, v)?;
            d.style = t.h.href(own)?;
            Kind::Text(d)
        }
        2 => {
            let mut d = text_body(t, v)?;
            let tail = attrib_tail(t, v)?;
            d.style = t.h.href(own)?;
            let (tag, flags) = tail.unwrap_or_default();
            Kind::Attrib { t: d, tag, flags }
        }
        3 => {
            let mut d = text_body(t, v)?;
            let Some((tag, flags)) = attrib_tail(t, v)? else {
                return Ok(None);
            };
            if v >= Version::R2010 {
                t.m.rc()?;
            }
            let prompt = t.tv()?;
            d.style = t.h.href(own)?;
            Kind::AttDef {
                t: d,
                tag,
                prompt,
                flags,
            }
        }
        4 => Kind::Block,
        5 => Kind::EndBlk,
        6 => Kind::SeqEnd,
        7 | 8 => insert(t, v, ty, own)?,
        74 => {
            let version = t.m.bs()?;
            if v >= Version::R2000 {
                t.m.bs()?;
            }
            let size = t.m.bl()? as usize;
            if size > t.m.left() / 8 {
                return Err(err("OLE2FRAME data length exceeds record"));
            }
            Kind::Ole { version }
        }
        10 => {
            let flags = t.m.rc()?;
            let p = t.m.p3()?;
            let mut w0 = t.m.bd()?;
            let w1 = if w0 < 0.0 {
                w0 = -w0;
                w0
            } else {
                t.m.bd()?
            };
            let bulge = t.m.bd()?;
            Kind::Vertex {
                flags,
                p,
                w0,
                w1,
                bulge,
            }
        }
        11..=13 => Kind::Vertex {
            flags: t.m.rc()?,
            p: t.m.p3()?,
            w0: 0.0,
            w1: 0.0,
            bulge: 0.0,
        },
        14 => {
            let mut f = [0i16; 4];
            for x in &mut f {
                *x = t.m.bs()? as i16;
            }
            Kind::FaceRec(f)
        }
        15 => {
            let flags = t.m.bs()?;
            t.m.bs()?;
            let w0 = t.m.bd()?;
            let w1 = t.m.bd()?;
            let th = t.m.bt(r2000)?;
            let elev = t.m.bd()?;
            let ext = t.m.be(r2000)?;
            let count = if v >= Version::R2004 {
                t.m.bl()? as usize
            } else {
                0
            };
            let owned = owned_list(t, v, own, count)?;
            Kind::Poly2d {
                flags,
                w0,
                w1,
                th,
                elev,
                ext,
                owned,
            }
        }
        16 | 29 | 30 => {
            let (flags, m, n) = match ty {
                16 => {
                    let sf = t.m.rc()?;
                    let closed = t.m.rc()?;
                    let mut f = 8 | (closed & 1) as i32;
                    if sf & 3 != 0 {
                        f |= 4;
                    }
                    (f, 0, 0)
                }
                29 => {
                    let nv = t.m.bs()?;
                    let nf = t.m.bs()?;
                    (64, nv, nf)
                }
                _ => {
                    let f = t.m.bs()?;
                    t.m.bs()?;
                    let m = t.m.bs()?;
                    let n = t.m.bs()?;
                    t.m.bs()?;
                    t.m.bs()?;
                    (16 | (f as i32 & 0x21), m, n)
                }
            };
            let count = if v >= Version::R2004 {
                t.m.bl()? as usize
            } else {
                0
            };
            let owned = owned_list(t, v, own, count)?;
            Kind::PolyN { flags, m, n, owned }
        }
        17 | 18 => {
            let c = t.m.p3()?;
            let r = t.m.bd()?;
            let th = t.m.bt(r2000)?;
            let ext = t.m.be(r2000)?;
            if ty == 18 {
                Kind::Circle { c, r, th, ext }
            } else {
                Kind::Arc {
                    c,
                    r,
                    th,
                    ext,
                    a0: t.m.bd()?,
                    a1: t.m.bd()?,
                }
            }
        }
        19 => {
            if !r2000 {
                Kind::Line {
                    a: t.m.p3()?,
                    b: t.m.p3()?,
                    th: t.m.bd()?,
                    ext: t.m.p3()?,
                }
            } else {
                let z_zero = t.m.b()?;
                let x1 = t.m.rd()?;
                let x2 = t.m.dd(x1)?;
                let y1 = t.m.rd()?;
                let y2 = t.m.dd(y1)?;
                let (z1, z2) = if z_zero {
                    (0.0, 0.0)
                } else {
                    let z1 = t.m.rd()?;
                    (z1, t.m.dd(z1)?)
                };
                Kind::Line {
                    a: [x1, y1, z1],
                    b: [x2, y2, z2],
                    th: t.m.bt(true)?,
                    ext: t.m.be(true)?,
                }
            }
        }
        20..=26 => read_dim(t, v, ty, own)?,
        27 => {
            let p = t.m.p3()?;
            let th = t.m.bt(r2000)?;
            let ext = t.m.be(r2000)?;
            Kind::Point { p, th, ext }
        }
        28 => {
            if !r2000 {
                let pts = [t.m.p3()?, t.m.p3()?, t.m.p3()?, t.m.p3()?];
                Kind::Face3d {
                    pts,
                    flags: t.m.bs()?,
                }
            } else {
                let no_flags = t.m.b()?;
                let z_zero = t.m.b()?;
                let x = t.m.rd()?;
                let y = t.m.rd()?;
                let z = if z_zero { 0.0 } else { t.m.rd()? };
                let p0 = [x, y, z];
                let mut pts = [p0; 4];
                for i in 1..4 {
                    let q = pts[i - 1];
                    pts[i] = [t.m.dd(q[0])?, t.m.dd(q[1])?, t.m.dd(q[2])?];
                }
                let flags = if no_flags { 0 } else { t.m.bs()? };
                Kind::Face3d { pts, flags }
            }
        }
        31 | 32 => {
            let th = t.m.bt(r2000)?;
            let elev = t.m.bd()?;
            let mut pts = [[0.0; 3]; 4];
            for p in &mut pts {
                let q = t.m.p2r()?;
                *p = [q[0], q[1], elev];
            }
            let ext = t.m.be(r2000)?;
            Kind::Solid {
                trace: ty == 32,
                pts,
                th,
                ext,
            }
        }
        34 => Kind::Viewport {
            c: t.m.p3()?,
            w: t.m.bd()?,
            h: t.m.bd()?,
        },
        35 => Kind::Ellipse {
            c: t.m.p3()?,
            major: t.m.p3()?,
            ext: t.m.p3()?,
            ratio: t.m.bd()?,
            a0: t.m.bd()?,
            a1: t.m.bd()?,
        },
        36 => read_spline(t, v)?,
        37..=39 => Kind::Acis {
            name: type_name(ty),
            sat: annot::acis(t)?,
        },
        45 => Kind::Leader(Box::new(annot::leader(t, v, own)?)),
        40 | 41 => Kind::Ray {
            xline: ty == 41,
            p: t.m.p3()?,
            v: t.m.p3()?,
        },
        44 => {
            let ins = t.m.p3()?;
            let ext = t.m.p3()?;
            let xdir = t.m.p3()?;
            let width = t.m.bd()?;
            if v >= Version::R2007 {
                t.m.bd()?;
            }
            let height = t.m.bd()?;
            let attach = t.m.bs()?;
            t.m.bs()?;
            t.m.bd()?;
            t.m.bd()?;
            let text = t.tv()?;
            let style = t.h.href(own)?;
            Kind::MText {
                ins,
                ext,
                xdir,
                width,
                height,
                attach,
                text,
                style,
            }
        }
        77 => lwpoly(t, v)?,
        78 => read_hatch(t, v)?,
        _ if ty >= 500 => match class {
            Some("LWPOLYLINE") | Some("LWPLINE") => lwpoly(t, v)?,
            Some("HATCH") => read_hatch(t, v)?,
            Some("MULTILEADER") => Kind::MLeader(Box::new(annot::mleader(t, v)?)),
            Some("ACIDBLOCKREFERENCE") => match insert(t, v, 7, own)? {
                Kind::Insert {
                    ins,
                    scale,
                    rot,
                    ext,
                    block,
                    ..
                } => Kind::BlockRef {
                    ins,
                    scale,
                    rot,
                    ext,
                    block,
                },
                k => k,
            },
            Some("ACAD_TABLE") => match insert(t, v, 7, own)? {
                Kind::Insert {
                    ins,
                    mut rot,
                    ext,
                    block,
                    ..
                } => {
                    // The horizontal direction follows the block-reference part; a value that
                    // does not read as a plane unit vector is ignored.
                    let dir = (|| -> R<P3> {
                        if v >= Version::R2010 {
                            t.m.rc()?;
                            t.m.bl()?;
                        }
                        t.m.bs()?;
                        t.m.p3()
                    })();
                    if let Ok(d) = dir {
                        let len = d[0].hypot(d[1]);
                        if d.iter().all(|x| x.is_finite())
                            && d[2].abs() < 1e-6
                            && (len - 1.0).abs() < 1e-3
                        {
                            rot = d[1].atan2(d[0]);
                        }
                    }
                    Kind::Table {
                        ins,
                        rot,
                        ext,
                        block,
                    }
                }
                k => k,
            },
            Some(c @ ("IMAGE" | "WIPEOUT")) => Kind::Image {
                wipeout: c == "WIPEOUT",
                d: Box::new(annot::image(t, v, own)?),
            },
            _ => return Ok(None),
        },
        _ => return Ok(None),
    }))
}

/// INSERT / MINSERT body (also the block-reference part of classes derived from INSERT).
fn insert(t: &mut Tri, v: Version, ty: u16, own: u64) -> R<Kind> {
    let ins = t.m.p3()?;
    let scale = if v < Version::R2000 {
        t.m.p3()?
    } else {
        match t.m.bb()? {
            3 => [1.0; 3],
            1 => [1.0, t.m.dd(1.0)?, t.m.dd(1.0)?],
            2 => {
                let x = t.m.rd()?;
                [x; 3]
            }
            _ => {
                let x = t.m.rd()?;
                [x, t.m.dd(x)?, t.m.dd(x)?]
            }
        }
    };
    let rot = t.m.bd()?;
    let ext = t.m.p3()?;
    let has_attr = t.m.b()?;
    let count = if v >= Version::R2004 && has_attr {
        t.m.bl()? as usize
    } else {
        0
    };
    let grid = if ty == 8 {
        Some((t.m.bs()?, t.m.bs()?, t.m.bd()?, t.m.bd()?))
    } else {
        None
    };
    let block = t.h.href(own)?;
    let attribs = if has_attr {
        owned_list(t, v, own, count)?
    } else {
        Owned::List(vec![])
    };
    Ok(Kind::Insert {
        ins,
        scale,
        rot,
        ext,
        block,
        attribs,
        grid,
    })
}

/// Fixed type codes that are entities (graphic objects).
pub(crate) fn is_entity_type(ty: u16) -> bool {
    matches!(ty, 1..=41 | 43..=47 | 74 | 77 | 78)
}

/// Kinds decoded on a best-effort basis: a record that does not decode is counted as
/// skipped (with a warning), not as failed.
pub(crate) fn best_effort(ty: u16, class: Option<&str>) -> bool {
    matches!(ty, 37..=39 | 45 | 74)
        || matches!(
            class,
            Some("MULTILEADER" | "IMAGE" | "WIPEOUT" | "ACIDBLOCKREFERENCE" | "ACAD_TABLE")
        )
}

/// Name of a fixed object type, for skip counts.
pub(crate) fn type_name(ty: u16) -> &'static str {
    match ty {
        9 => "OBJECT9",
        33 => "SHAPE",
        37 => "REGION",
        38 => "3DSOLID",
        39 => "BODY",
        43 => "OLEFRAME",
        45 => "LEADER",
        46 => "TOLERANCE",
        47 => "MLINE",
        74 => "OLE2FRAME",
        _ => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwg::bits::BitW;

    fn frame_of(w: &BitW, data_end: usize, handle: u64) -> Frame<'_> {
        Frame {
            handle,
            ty: 19,
            payload: &w.d,
            r: Bits::new(&w.d),
            data_end: Some(data_end),
            crc_ok: true,
        }
    }

    /// R2004 entity common data with the given colour word (and RGB value when `rgb`).
    fn r2004_entity(word: u16, rgb: Option<u32>, book: bool) -> (BitW, usize) {
        let mut w = BitW::default();
        w.b(false).bb(2).bl(0).b(true).bs(word);
        if let Some(v) = rgb {
            w.bl(v);
        }
        w.bd(2.5).bb(0).bb(0).bs(0).rc(7);
        let data_end = w.n;
        if book {
            w.h(5, 0x77);
        }
        w.h(5, 0x10);
        (w, data_end)
    }

    /// Layer record body as stored before R2000 (flag bits) or from R2000 (flag word).
    fn layer_rec(ver: Version, bits: [bool; 4], word: u16, color: i16) -> LayerRec {
        let mut w = BitW::default();
        w.tv("L").b(false);
        if ver < Version::R2007 {
            w.bs(0);
        }
        w.b(false);
        if ver <= Version::R14 {
            for b in bits {
                w.b(b);
            }
        } else {
            w.bs(word);
        }
        w.bs(color as u16);
        let mut h = BitW::default();
        h.h(5, 1);
        if ver >= Version::R2000 {
            h.h(5, 2);
        }
        h.h(5, 3);
        let mut t = Tri {
            m: Bits::new(&w.d),
            s: None,
            h: Bits::new(&h.d),
            h_inline: false,
            unicode: false,
            cp: None,
        };
        layer(&mut t, ver, 0x50).unwrap()
    }

    #[test]
    fn layer_flags_r14_and_r2000() {
        for ver in [Version::R13, Version::R14] {
            // frozen, on-bit, frozen-in-new, locked
            let l = layer_rec(ver, [false, false, false, false], 0, 7);
            assert!(!l.off && l.flags == 0 && l.color == 7, "{ver:?} visible");
            let l = layer_rec(ver, [false, true, false, false], 0, 7);
            assert!(!l.off, "{ver:?} on-bit set is not hidden");
            let l = layer_rec(ver, [false, false, false, false], 0, -7);
            assert!(l.off && l.color == 7, "{ver:?} negative colour is off");
            let l = layer_rec(ver, [true, false, true, false], 0, 3);
            assert!(!l.off && l.flags == 3, "{ver:?} frozen");
            let l = layer_rec(ver, [false, false, false, true], 0, 3);
            assert!(!l.off && l.flags == 4, "{ver:?} locked");
        }
        let l = layer_rec(Version::R2000, [false; 4], 0, 7);
        assert!(!l.off && l.flags == 0);
        let l = layer_rec(Version::R2000, [false; 4], 2, -7);
        assert!(l.off && l.color == 7);
        let l = layer_rec(Version::R2000, [false; 4], 1, 7);
        assert!(!l.off && l.flags == 1, "frozen");
        let l = layer_rec(Version::R2000, [false; 4], 8, 7);
        assert!(!l.off && l.flags == 4, "locked");
    }

    #[test]
    fn color_book_reference_reads_no_rgb() {
        let (w, de) = r2004_entity(0xC000 | 5, None, true);
        let (_, c) = open_entity(&frame_of(&w, de, 0x40), Version::R2004, None).unwrap();
        assert_eq!((c.color, c.true_color), (5, None));
        assert_eq!(c.ltscale, 2.5);
        assert_eq!(c.lw_index, Some(7));
        assert_eq!(c.layer, 0x10);
        // RGB flag alone: the colour value is read and the stream stays aligned.
        let (w, de) = r2004_entity(0x8000 | 5, Some(0xC2FF_8000), false);
        let (_, c) = open_entity(&frame_of(&w, de, 0x40), Version::R2004, None).unwrap();
        assert_eq!(c.true_color, Some(0xFF_8000));
        assert_eq!((c.ltscale, c.layer), (2.5, 0x10));
    }

    fn r2000_entity(nolinks: bool) -> (BitW, usize) {
        let mut w = BitW::default();
        w.b(false)
            .bb(0)
            .bl(0)
            .b(nolinks)
            .bs(3)
            .bd(1.0)
            .bb(0)
            .bb(0)
            .bs(0)
            .rc(0);
        let data_end = w.n;
        w.h(4, 0x10).h(3, 0);
        if !nolinks {
            w.h(4, 0x20).h(4, 0x22);
        }
        w.h(5, 0x30);
        (w, data_end)
    }

    #[test]
    fn r2000_sibling_links_kept() {
        let (w, de) = r2000_entity(false);
        let (_, c) = open_entity(&frame_of(&w, de, 0x21), Version::R2000, None).unwrap();
        assert_eq!((c.owner, c.next, c.layer), (0x10, 0x22, 0x30));
        let (w, de) = r2000_entity(true);
        let (_, c) = open_entity(&frame_of(&w, de, 0x21), Version::R2000, None).unwrap();
        assert_eq!((c.next, c.layer), (0x22, 0x30));
    }

    #[test]
    fn dictionary_entries() {
        let mut w = BitW::default();
        w.bl(0).b(true).bl(2).bs(1).rc(0).tv("Model").tv("Layout1");
        let de = w.n;
        w.h(4, 0x0C).h(2, 0x100).h(2, 0x101);
        let f = frame_of(&w, de, 0x1A);
        let mut t = open_object(&f, Version::R2004, None).unwrap();
        let d = dictionary(&mut t, Version::R2004, 0x1A).unwrap();
        assert_eq!(d, vec![("Model".into(), 0x100), ("Layout1".into(), 0x101)]);
    }

    #[test]
    fn prealloc_bounded_by_stream() {
        assert_eq!(room(usize::MAX, 1 << 20, 128), (1 << 20) / 128);
        assert_eq!(room(10, 1 << 30, 4), 10);
        assert_eq!(room(usize::MAX, usize::MAX, 1), MAX_PREALLOC);
        // A hostile LWPOLYLINE point count: Err, not a huge allocation.
        let mut w = BitW::default();
        w.bs(0).bl(0x7FFF_FFFF);
        let f = frame_of(&w, w.n, 1);
        let mut t = f.tri(Bits::new(&w.d), w.n, Version::R2004, None).unwrap();
        assert!(lwpoly(&mut t, Version::R2004).is_err());
    }

    fn text_record(df: u8, x: f64) -> BitW {
        let mut w = BitW::default();
        let rd = |w: &mut BitW, v: f64| {
            for b in v.to_le_bytes() {
                w.rc(b);
            }
        };
        w.rc(df);
        rd(&mut w, x);
        rd(&mut w, 2.0);
        w.b(true).b(true);
        rd(&mut w, 0.5);
        w.tv("u");
        w
    }

    #[test]
    fn text_nonfinite_insertion() {
        // All optional fields omitted (0xFD keeps the alignment flag clear).
        let w = text_record(0xFF, f64::NAN);
        let f = frame_of(&w, w.n, 1);
        let mut t = f.tri(Bits::new(&w.d), w.n, Version::R2004, None).unwrap();
        let d = text_body(&mut t, Version::R2004).unwrap();
        assert_eq!(
            (d.ins, d.height, d.text.as_str()),
            ([0.0, 0.0, 0.0], 0.5, "u")
        );
        let w = text_record(0xFF, 3.0);
        let f = frame_of(&w, w.n, 1);
        let mut t = f.tri(Bits::new(&w.d), w.n, Version::R2004, None).unwrap();
        assert_eq!(
            text_body(&mut t, Version::R2004).unwrap().ins,
            [3.0, 2.0, 0.0]
        );
        let w = text_record(0xFD, f64::NAN);
        let f = frame_of(&w, w.n, 1);
        let mut t = f.tri(Bits::new(&w.d), w.n, Version::R2004, None).unwrap();
        assert!(text_body(&mut t, Version::R2004).is_err());
    }

    #[test]
    fn owned_list_chain_before_r2004() {
        let mut w = BitW::default();
        w.h(4, 0x50).h(4, 0x58);
        let f = frame_of(&w, 0, 1);
        let mut t = f.tri(Bits::new(&w.d), 0, Version::R2000, None).unwrap();
        assert_eq!(
            owned_list(&mut t, Version::R2000, 1, 0).unwrap(),
            Owned::Chain(0x50, 0x58)
        );
    }

    #[test]
    fn acid_block_reference_reads_insert_part() {
        let mut w = BitW::default();
        w.bd(5.0).bd(6.0).bd(0.0); // insertion point
        w.bb(3); // unit scale
        w.bd(std::f64::consts::FRAC_PI_2).bd(0.0).bd(0.0).bd(1.0);
        w.b(false); // no attributes
        w.bl(7).bd(9.0); // subclass data after the block-reference part: ignored
        let n = w.n;
        w.h(5, 0x30);
        let f = frame_of(&w, n, 0x40);
        let mut t = f.tri(Bits::new(&w.d), n, Version::R2004, None).unwrap();
        let k = entity(
            &mut t,
            Version::R2004,
            501,
            Some("ACIDBLOCKREFERENCE"),
            0x40,
        )
        .unwrap();
        let Some(Kind::BlockRef {
            ins,
            scale,
            rot,
            ext,
            block,
        }) = k
        else {
            panic!("not a block reference")
        };
        assert_eq!(
            (ins, scale, ext),
            ([5.0, 6.0, 0.0], [1.0; 3], [0.0, 0.0, 1.0])
        );
        assert_eq!(block, 0x30);
        assert!((rot - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!(best_effort(501, Some("ACIDBLOCKREFERENCE")));
    }

    #[test]
    fn acad_table_reads_block_and_direction() {
        let mut w = BitW::default();
        w.bd(5.0).bd(6.0).bd(0.0); // insertion point
        w.bb(3); // unit scale
        w.bd(0.0).bd(0.0).bd(0.0).bd(1.0);
        w.b(false); // no attributes
        w.bs(1).bd(0.0).bd(1.0).bd(0.0); // flag, horizontal direction (0, 1, 0)
        let n = w.n;
        w.h(5, 0x30);
        let f = frame_of(&w, n, 0x40);
        let mut t = f.tri(Bits::new(&w.d), n, Version::R2004, None).unwrap();
        let k = entity(&mut t, Version::R2004, 502, Some("ACAD_TABLE"), 0x40).unwrap();
        let Some(Kind::Table {
            ins, rot, block, ..
        }) = k
        else {
            panic!("not a table")
        };
        assert_eq!((ins, block), ([5.0, 6.0, 0.0], 0x30));
        assert!((rot - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!(best_effort(502, Some("ACAD_TABLE")));
    }

    #[test]
    fn ole2frame_frame_kept_payload_checked() {
        let mut w = BitW::default();
        w.bs(2).bs(0).bl(3).rc(1).rc(2).rc(3).rc(0);
        let f = frame_of(&w, w.n, 1);
        let mut t = f.tri(Bits::new(&w.d), w.n, Version::R2004, None).unwrap();
        let k = entity(&mut t, Version::R2004, 74, None, 1).unwrap();
        assert!(matches!(k, Some(Kind::Ole { version: 2 })), "{k:?}");
        // Declared payload longer than the record: rejected (counted as skipped, best effort).
        let mut w = BitW::default();
        w.bs(2).bs(0).bl(4096).rc(1);
        let f = frame_of(&w, w.n, 1);
        let mut t = f.tri(Bits::new(&w.d), w.n, Version::R2004, None).unwrap();
        assert!(entity(&mut t, Version::R2004, 74, None, 1).is_err());
        assert!(best_effort(74, None));
    }
}
