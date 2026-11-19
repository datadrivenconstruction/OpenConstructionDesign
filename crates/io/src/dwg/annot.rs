//! Annotation and opaque entity bodies: LEADER (45), MULTILEADER, IMAGE / WIPEOUT and
//! IMAGEDEF (class records), REGION / 3DSOLID / BODY (37–39). Spec: docs/specs/DWG.md §8.
//! Each decoder returns `Err` on an inconsistency; the caller keeps the record as skipped.

use super::bits::Bits;
use super::header::Tri;
use super::objects::{P2, P3};
use super::{err, Version, R};

/// Upper bound on vertices of one leader line / clip boundary.
const MAX_POINTS: usize = 1 << 16;
/// Upper bound on leader roots and lines of one multileader.
const MAX_LEADERS: usize = 4096;
/// Largest inline modeler text kept for one record, bytes.
pub(crate) const MAX_SAT: usize = 8 << 20;

/// `n` points of 3BD (at least 6 bits each), bounded by the bits left.
fn pts3(m: &mut Bits, n: usize) -> R<Vec<P3>> {
    if n > MAX_POINTS || n.saturating_mul(6) > m.left() {
        return Err(err("point count exceeds record"));
    }
    (0..n).map(|_| m.p3()).collect()
}

/// BL count bounded by the bits left, `min_bits` per item.
fn count(m: &mut Bits, max: usize, min_bits: usize) -> R<usize> {
    let n = m.bl()? as usize;
    if n > max || n.saturating_mul(min_bits) > m.left() {
        return Err(err("item count exceeds record"));
    }
    Ok(n)
}

// ---------------------------------------------------------------- LEADER

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LeaderD {
    /// DXF 73: 0 text, 1 tolerance, 2 block, 3 none.
    pub annot: u16,
    /// DXF 72: 0 straight, 1 spline.
    pub path: u16,
    pub pts: Vec<P3>,
    pub ext: P3,
    pub xdir: P3,
    pub block_off: P3,
    pub annot_off: P3,
    pub box_h: f64,
    pub box_w: f64,
    pub hook: bool,
    pub arrow: bool,
    /// Dimension style record.
    pub style: u64,
}

type LeaderTail = (P3, P3, f64, f64, bool, bool);

/// Fields after the horizontal direction; `boxed` = text box height/width stored.
fn leader_tail(m: &mut Bits, v: Version, boxed: bool) -> R<LeaderTail> {
    let block_off = if v >= Version::R14 { m.p3()? } else { [0.0; 3] };
    let annot_off = if v >= Version::R2000 {
        m.p3()?
    } else {
        [0.0; 3]
    };
    if v <= Version::R14 {
        m.bd()?; // dimension gap
    }
    let (bh, bw) = if boxed {
        (m.bd()?, m.bd()?)
    } else {
        (0.0, 0.0)
    };
    let hook = m.b()?;
    let arrow = m.b()?;
    if v <= Version::R14 {
        // arrow type, arrow size, two flags, a word, by-block colour, two flags
        m.bs()?;
        m.bd()?;
        m.b()?;
        m.b()?;
        m.bs()?;
        m.bs()?;
        m.b()?;
        m.b()?;
    } else {
        m.bs()?;
        m.b()?;
        m.b()?;
    }
    Ok((block_off, annot_off, bh, bw, hook, arrow))
}

pub(crate) fn leader(t: &mut Tri, v: Version, own: u64) -> R<LeaderD> {
    let m = &mut t.m;
    m.b()?;
    let annot = m.bs()?;
    let path = m.bs()?;
    let n = m.bl()? as usize;
    if n == 0 {
        return Err(err("leader without vertices"));
    }
    let pts = pts3(m, n)?;
    m.p3()?; // origin of the leader plane
    let ext = m.p3()?;
    let xdir = m.p3()?;
    // The text box size is stored before R2010; when a record disagrees, the variant that
    // ends exactly at the data boundary wins, otherwise the generation's layout is used.
    let usual = v < Version::R2010;
    let mut pick: Option<(Bits, LeaderTail)> = None;
    for boxed in [usual, !usual] {
        let mut r = m.clone();
        let Ok(tail) = leader_tail(&mut r, v, boxed) else {
            continue;
        };
        let exact = r.left() == 0;
        if exact || (boxed == usual && pick.is_none()) {
            pick = Some((r, tail));
        }
        if exact {
            break;
        }
    }
    let (r, (block_off, annot_off, box_h, box_w, hook, arrow)) =
        pick.ok_or_else(|| err("leader record truncated"))?;
    *m = r;
    // Annotation, then dimension style; a short handle stream keeps the geometry.
    let _ = t.h.h();
    let style = t.h.href(own).unwrap_or(0);
    Ok(LeaderD {
        annot,
        path,
        pts,
        ext,
        xdir,
        block_off,
        annot_off,
        box_h,
        box_w,
        hook,
        arrow,
        style,
    })
}

// ---------------------------------------------------------------- MULTILEADER

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct MlRoot {
    /// Connection (last leader line) point and dogleg direction.
    pub conn: P3,
    pub dir: P3,
    pub index: u32,
    /// Dogleg length.
    pub landing: f64,
    pub lines: Vec<Vec<P3>>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct MlText {
    pub text: String,
    pub normal: P3,
    pub loc: P3,
    pub dir: P3,
    pub rot: f64,
    pub width: f64,
    pub height: f64,
    pub align: u16,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct MLeaderD {
    pub roots: Vec<MlRoot>,
    pub scale: f64,
    pub base: P3,
    pub text_h: f64,
    pub arrow: f64,
    pub gap: f64,
    pub text: Option<MlText>,
    /// Block content: location, scale, rotation.
    pub block: Option<(P3, P3, f64)>,
    pub leader_type: u16,
    pub dogleg: bool,
    /// 1 block, 2 text.
    pub content_type: u16,
}

/// BL count of (3BD start, 3BD end) break pairs.
fn breaks(m: &mut Bits) -> R<()> {
    let n = count(m, MAX_POINTS, 12)?;
    for _ in 0..n {
        m.p3()?;
        m.p3()?;
    }
    Ok(())
}

/// Entity-level settings after the context data: (leader type, dogleg, content type).
fn ml_settings(t: &mut Tri, v: Version) -> R<(u16, bool, u16)> {
    t.m.bl()?; // override flags
    let lt = t.m.bs()?;
    t.cmc(v)?;
    t.m.bl()?; // line weight
    t.m.b()?; // landing enabled
    let dogleg = t.m.b()?;
    t.m.bd()?; // landing distance
    t.m.bd()?; // arrow size
    Ok((lt, dogleg, t.m.bs()?))
}

pub(crate) fn mleader(t: &mut Tri, v: Version) -> R<MLeaderD> {
    let r2010 = v >= Version::R2010;
    if r2010 && t.m.bs()? > 16 {
        return Err(err("multileader class version"));
    }
    let mut d = MLeaderD::default();
    let n = count(&mut t.m, MAX_LEADERS, 20)?;
    let mut line_handles = 0usize;
    for _ in 0..n {
        let mut root = MlRoot::default();
        t.m.b()?; // content valid
        t.m.b()?;
        root.conn = t.m.p3()?;
        root.dir = t.m.p3()?;
        breaks(&mut t.m)?;
        root.index = t.m.bl()?;
        root.landing = t.m.bd()?;
        let nl = count(&mut t.m, MAX_LEADERS, 6)?;
        for _ in 0..nl {
            let np = t.m.bl()? as usize;
            let line = pts3(&mut t.m, np)?;
            let nb = count(&mut t.m, MAX_POINTS, 4)?;
            for _ in 0..nb {
                t.m.bl()?; // segment index
                breaks(&mut t.m)?;
            }
            t.m.bl()?; // line index
            if r2010 {
                t.m.bs()?; // leader type
                t.cmc(v)?;
                t.m.bl()?; // line weight
                t.m.bd()?; // arrow size
                t.m.bl()?; // override flags
                line_handles += 2;
            }
            root.lines.push(line);
        }
        if r2010 {
            t.m.bs()?; // attachment direction
        }
        d.roots.push(root);
    }
    d.scale = t.m.bd()?;
    d.base = t.m.p3()?;
    d.text_h = t.m.bd()?;
    d.arrow = t.m.bd()?;
    d.gap = t.m.bd()?;
    t.m.bs()?; // left attachment
    t.m.bs()?; // right attachment
    t.m.bs()?; // text alignment type
    t.m.bs()?; // attachment type
    if t.m.b()? {
        let text = t.tv()?;
        let normal = t.m.p3()?;
        let loc = t.m.p3()?;
        let dir = t.m.p3()?;
        let rot = t.m.bd()?;
        let width = t.m.bd()?;
        let height = t.m.bd()?;
        t.m.bd()?; // line spacing factor
        t.m.bs()?; // line spacing style
        t.cmc(v)?;
        let align = t.m.bs()?;
        t.m.bs()?; // flow direction
        t.cmc(v)?; // background colour
        t.m.bd()?;
        t.m.bl()?;
        t.m.b()?;
        t.m.b()?;
        t.m.bs()?; // column type
        t.m.b()?;
        t.m.bd()?;
        t.m.bd()?;
        t.m.b()?;
        let nc = count(&mut t.m, MAX_POINTS, 2)?;
        for _ in 0..nc {
            t.m.bd()?;
        }
        t.m.b()?; // word break
        t.m.b()?;
        d.text = Some(MlText {
            text,
            normal,
            loc,
            dir,
            rot,
            width,
            height,
            align,
        });
    } else if t.m.b()? {
        t.m.p3()?; // normal
        let loc = t.m.p3()?;
        let scale = t.m.p3()?;
        let rot = t.m.bd()?;
        t.cmc(v)?;
        for _ in 0..16 {
            t.m.bd()?;
        }
        d.block = Some((loc, scale, rot));
    }
    t.m.p3()?; // plane origin, x axis, y axis
    t.m.p3()?;
    t.m.p3()?;
    t.m.b()?;
    if r2010 {
        t.m.bs()?;
        t.m.bs()?;
    }
    let default = (1, true, if d.text.is_some() { 2 } else { 1 });
    (d.leader_type, d.dogleg, d.content_type) = ml_settings(t, v).unwrap_or(default);
    for _ in 0..line_handles {
        if t.h.h().is_err() {
            break;
        }
    }
    Ok(d)
}

// ---------------------------------------------------------------- IMAGE / WIPEOUT

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImageD {
    pub origin: P3,
    /// One pixel along the image u and v axes.
    pub u: P3,
    pub v: P3,
    /// Size in pixels.
    pub size: P2,
    pub flags: u16,
    pub clip: bool,
    pub bright: u8,
    pub contrast: u8,
    pub fade: u8,
    /// 1 rectangle, 2 polygon.
    pub clip_type: u16,
    pub clip_pts: Vec<P2>,
    /// IMAGEDEF object.
    pub def: u64,
}

pub(crate) fn image(t: &mut Tri, v: Version, own: u64) -> R<ImageD> {
    let m = &mut t.m;
    m.bl()?; // class version
    let origin = m.p3()?;
    let u = m.p3()?;
    let vv = m.p3()?;
    let size = m.p2r()?;
    let flags = m.bs()?;
    let clip = m.b()?;
    let bright = m.rc()?;
    let contrast = m.rc()?;
    let fade = m.rc()?;
    if v >= Version::R2010 {
        m.b()?; // clip mode
    }
    let clip_type = m.bs()?;
    let n = if clip_type == 1 {
        2
    } else {
        count(m, MAX_POINTS, 128)?
    };
    let clip_pts = (0..n).map(|_| m.p2r()).collect::<R<Vec<_>>>()?;
    let def = t.h.href(own).unwrap_or(0);
    Ok(ImageD {
        origin,
        u,
        v: vv,
        size,
        flags,
        clip,
        bright,
        contrast,
        fade,
        clip_type,
        clip_pts,
        def,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImageDef {
    pub size: P2,
    pub path: String,
    pub loaded: bool,
    pub units: u8,
    pub pixel: P2,
}

/// IMAGEDEF object body (streams opened by `open_object`).
pub(crate) fn imagedef(t: &mut Tri) -> R<ImageDef> {
    t.m.bl()?; // class version
    let size = t.m.p2r()?;
    let path = t.tv()?;
    let loaded = t.m.b()?;
    let units = t.m.rc()?;
    let pixel = t.m.p2r()?;
    Ok(ImageDef {
        size,
        path,
        loaded,
        units,
        pixel,
    })
}

// ---------------------------------------------------------------- REGION / 3DSOLID / BODY

/// Modeler text of a REGION / 3DSOLID / BODY when stored inline as text (version 1),
/// decoded; `None` for an empty record, binary or out-of-record storage, or oversize text.
pub(crate) fn acis(t: &mut Tri) -> R<Option<String>> {
    let m = &mut t.m;
    if m.b()? {
        return Ok(None);
    }
    m.b()?;
    if m.bs()? != 1 {
        return Ok(None);
    }
    let mut out: Vec<u8> = vec![];
    loop {
        let n = m.bl()? as usize;
        if n == 0 {
            break;
        }
        if n > m.left() / 8 {
            return Err(err("modeler data exceeds record"));
        }
        if out.len() + n > MAX_SAT {
            return Ok(None);
        }
        out.reserve(n);
        for _ in 0..n {
            let c = m.rc()?;
            out.push(if c <= 32 { c } else { 159u8.wrapping_sub(c) });
        }
    }
    Ok(Some(String::from_utf8_lossy(&out).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwg::bits::BitW;

    fn tri<'a>(d: &'a [u8], data_end: usize, s: Option<&'a [u8]>) -> Tri<'a> {
        Tri {
            m: Bits::view(d, 0, data_end),
            s: s.map(Bits::new),
            h: Bits::view(d, data_end, d.len() * 8),
            h_inline: false,
            unicode: s.is_some(),
            cp: None,
        }
    }

    fn p3(w: &mut BitW, p: P3) {
        w.bd(p[0]).bd(p[1]).bd(p[2]);
    }

    fn rd(w: &mut BitW, v: f64) {
        for b in v.to_le_bytes() {
            w.rc(b);
        }
    }

    /// Colour (index, RGB word, no names), R2004+.
    fn cmc(w: &mut BitW) {
        w.bs(256).bl(0).rc(0);
    }

    fn tu(s: &str) -> BitW {
        let mut w = BitW::default();
        let u: Vec<u16> = s.encode_utf16().collect();
        w.bs(u.len() as u16);
        for x in u {
            w.rc(x as u8).rc((x >> 8) as u8);
        }
        w
    }

    const PTS: [P3; 3] = [[0.0, 0.0, 0.0], [10.0, 5.0, 0.0], [14.0, 5.0, 0.0]];

    /// LEADER body up to the horizontal direction.
    fn leader_head(w: &mut BitW) {
        w.b(false).bs(0).bs(0).bl(3);
        for q in PTS {
            p3(w, q);
        }
        p3(w, [0.0; 3]);
        p3(w, [0.0, 0.0, 1.0]);
        p3(w, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn leader_r2018_without_text_box() {
        let mut w = BitW::default();
        leader_head(&mut w);
        p3(&mut w, [0.0; 3]);
        p3(&mut w, [0.5, 0.0, 0.0]);
        w.b(false).b(true).bs(0).b(false).b(false);
        let de = w.n;
        w.h(5, 0x2A).h(5, 0x27);
        let d = leader(&mut tri(&w.d, de, None), Version::R2018, 0x40).unwrap();
        assert_eq!(d.pts, PTS.to_vec());
        assert_eq!((d.arrow, d.hook, d.style), (true, false, 0x27));
        assert_eq!((d.box_h, d.box_w, d.annot_off), (0.0, 0.0, [0.5, 0.0, 0.0]));
    }

    #[test]
    fn leader_r2000_text_box_and_boundary_choice() {
        let mut w = BitW::default();
        leader_head(&mut w);
        p3(&mut w, [0.0; 3]);
        p3(&mut w, [0.0; 3]);
        w.bd(2.5).bd(12.0);
        w.b(true).b(true).bs(0).b(false).b(false);
        let de = w.n;
        w.h(5, 0x10).h(5, 0x11);
        let d = leader(&mut tri(&w.d, de, None), Version::R2000, 1).unwrap();
        assert_eq!((d.box_h, d.box_w, d.hook, d.style), (2.5, 12.0, true, 0x11));
        // Same generation written without the box: only that variant ends at the boundary.
        let mut w = BitW::default();
        leader_head(&mut w);
        p3(&mut w, [0.0; 3]);
        p3(&mut w, [0.0; 3]);
        w.b(false).b(true).bs(0).b(false).b(false);
        let de = w.n;
        let d = leader(&mut tri(&w.d, de, None), Version::R2004, 1).unwrap();
        assert_eq!((d.box_h, d.arrow, d.pts.len()), (0.0, true, 3));
        // A hostile vertex count is refused, not allocated.
        let mut w = BitW::default();
        w.b(false).bs(0).bs(0).bl(0x7FFF_FFFF);
        assert!(leader(&mut tri(&w.d, w.n, None), Version::R2018, 1).is_err());
    }

    #[test]
    fn mleader_r2018_text_content() {
        let mut w = BitW::default();
        w.bs(2).bl(1);
        // root: flags, connection point, direction, no breaks, index, dogleg length
        w.b(true).b(false);
        p3(&mut w, [14.0, 5.0, 0.0]);
        p3(&mut w, [1.0, 0.0, 0.0]);
        w.bl(0).bl(0).bd(2.0).bl(1);
        // one leader line with two points
        w.bl(2);
        p3(&mut w, [0.0, 0.0, 0.0]);
        p3(&mut w, [10.0, 5.0, 0.0]);
        w.bl(0).bl(0).bs(1);
        cmc(&mut w);
        w.bl(0).bd(0.18).bl(0);
        w.bs(0);
        // context
        w.bd(1.0);
        p3(&mut w, [16.0, 5.0, 0.0]);
        w.bd(2.5).bd(0.18).bd(0.09);
        w.bs(1).bs(1).bs(0).bs(0);
        w.b(true);
        p3(&mut w, [0.0, 0.0, 1.0]);
        p3(&mut w, [16.5, 6.0, 0.0]);
        p3(&mut w, [1.0, 0.0, 0.0]);
        w.bd(0.0).bd(20.0).bd(0.0).bd(1.0).bs(1);
        cmc(&mut w);
        w.bs(1).bs(5);
        cmc(&mut w);
        w.bd(1.5).bl(0).b(false).b(false);
        w.bs(0)
            .b(true)
            .bd(0.0)
            .bd(0.0)
            .b(false)
            .bl(0)
            .b(true)
            .b(false);
        p3(&mut w, [0.0; 3]);
        p3(&mut w, [1.0, 0.0, 0.0]);
        p3(&mut w, [0.0, 1.0, 0.0]);
        w.b(false).bs(1).bs(9);
        // settings: flags, leader type, colour, weight, landing, dogleg, lengths, content
        w.bl(0).bs(1);
        cmc(&mut w);
        w.bl(0).b(true).b(true).bd(2.0).bd(0.18).bs(2);
        let de = w.n;
        w.h(5, 0x14).h(5, 0x15).h(5, 0x11);
        let s = tu("Note");
        let d = mleader(&mut tri(&w.d, de, Some(&s.d)), Version::R2018).unwrap();
        assert_eq!(d.roots.len(), 1);
        let r = &d.roots[0];
        assert_eq!((r.conn, r.landing), ([14.0, 5.0, 0.0], 2.0));
        assert_eq!(r.lines, vec![vec![[0.0; 3], [10.0, 5.0, 0.0]]]);
        let x = d.text.as_ref().unwrap();
        assert_eq!(
            (x.text.as_str(), x.loc, x.width),
            ("Note", [16.5, 6.0, 0.0], 20.0)
        );
        assert_eq!(
            (d.text_h, d.arrow, d.content_type, d.dogleg),
            (2.5, 0.18, 2, true)
        );
        // Truncated record: refused.
        let short = &w.d[..w.d.len() / 3];
        assert!(mleader(&mut tri(short, short.len() * 8, Some(&s.d)), Version::R2018).is_err());
    }

    #[test]
    fn image_and_definition() {
        let mut w = BitW::default();
        w.bl(0);
        p3(&mut w, [100.0, 50.0, 0.0]);
        p3(&mut w, [0.5, 0.0, 0.0]);
        p3(&mut w, [0.0, 0.5, 0.0]);
        rd(&mut w, 640.0);
        rd(&mut w, 480.0);
        w.bs(7).b(false).rc(50).rc(60).rc(0).b(false).bs(1);
        for v in [-0.5, -0.5, 639.5, 479.5] {
            rd(&mut w, v);
        }
        let de = w.n;
        w.h(5, 0x30).h(3, 0x31);
        let d = image(&mut tri(&w.d, de, None), Version::R2018, 0x2F).unwrap();
        assert_eq!(
            (d.origin, d.size, d.def),
            ([100.0, 50.0, 0.0], [640.0, 480.0], 0x30)
        );
        assert_eq!((d.flags, d.bright, d.contrast, d.clip_type), (7, 50, 60, 1));
        assert_eq!(d.clip_pts, vec![[-0.5, -0.5], [639.5, 479.5]]);

        let mut w = BitW::default();
        w.bl(0);
        rd(&mut w, 640.0);
        rd(&mut w, 480.0);
        w.tv("C:\\img\\site.png").b(true).rc(2);
        rd(&mut w, 0.01);
        rd(&mut w, 0.01);
        let de = w.n;
        let d = imagedef(&mut tri(&w.d, de, None)).unwrap();
        assert_eq!(
            (d.path.as_str(), d.size, d.loaded),
            ("C:\\img\\site.png", [640.0, 480.0], true)
        );
    }

    #[test]
    fn acis_inline_text_decoded() {
        let sat = "400 0 1 0\npoint $-1 1 2 3 #\n";
        let mut w = BitW::default();
        w.b(false).b(false).bs(1).bl(sat.len() as u32);
        for c in sat.bytes() {
            w.rc(if c <= 32 { c } else { 159 - c });
        }
        w.bl(0);
        let de = w.n;
        assert_eq!(
            acis(&mut tri(&w.d, de, None)).unwrap().as_deref(),
            Some(sat)
        );
        let mut w = BitW::default();
        w.b(true);
        assert_eq!(acis(&mut tri(&w.d, w.n, None)).unwrap(), None);
        // Binary storage (version 2): kept opaque.
        let mut w = BitW::default();
        w.b(false).b(false).bs(2);
        assert_eq!(acis(&mut tri(&w.d, w.n, None)).unwrap(), None);
    }
}
