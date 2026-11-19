//! Compact binary form of an [`Underlay`] for the project file: layer names interned, the
//! whole record deflated. Round trip is exact (f64 bits kept).

use std::collections::{BTreeMap, HashMap};

use super::blocks::{BlockSrc, UBlocks, URef};
use super::source::{Pen, UArc, UBlockHint, UFill, UInsert, ULayer, ULine, UPoly, UText, Underlay};
use super::textfmt::TextFmt;
use crate::dxf::{
    self as dx, AttDef, Attrib, Block, BlockKind, Common, Entity, HeaderVar, Insert, Layer,
    LineType, LwVertex, Pair, TextStyle,
};

const MAGIC: &[u8; 4] = b"DDCU";
const FORMAT: u32 = 1;
/// Refuse to inflate past this: a damaged blob must not take all memory.
const MAX_RAW: usize = 1 << 31;

type P = (f64, f64);

struct W {
    b: Vec<u8>,
    names: HashMap<String, u32>,
    table: Vec<String>,
}

impl W {
    fn u8(&mut self, v: u8) {
        self.b.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn len(&mut self, n: usize) {
        self.u32(n as u32);
    }
    fn f32(&mut self, v: f32) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn p(&mut self, p: P) {
        self.f64(p.0);
        self.f64(p.1);
    }
    fn pts(&mut self, v: &[P]) {
        self.len(v.len());
        v.iter().for_each(|&q| self.p(q));
    }
    fn str(&mut self, s: &str) {
        self.len(s.len());
        self.b.extend_from_slice(s.as_bytes());
    }
    fn rgb(&mut self, c: Option<[u8; 3]>) {
        match c {
            Some(c) => {
                self.u8(1);
                self.b.extend_from_slice(&c);
            }
            None => self.u8(0),
        }
    }
    /// Interned name (layers repeat on every primitive).
    fn name(&mut self, s: &str) {
        let k = match self.names.get(s) {
            Some(&k) => k,
            None => {
                let k = self.table.len() as u32;
                self.names.insert(s.to_string(), k);
                self.table.push(s.to_string());
                k
            }
        };
        self.u32(k);
    }
    fn i32(&mut self, v: i32) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn ostr(&mut self, s: &Option<String>) {
        match s {
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
            None => self.u8(0),
        }
    }
    fn oi32(&mut self, v: Option<i32>) {
        match v {
            Some(v) => {
                self.u8(1);
                self.i32(v);
            }
            None => self.u8(0),
        }
    }
    fn pt3(&mut self, p: &[f64; 3]) {
        p.iter().for_each(|&v| self.f64(v));
    }
    fn f64s(&mut self, v: &[f64]) {
        self.len(v.len());
        v.iter().for_each(|&x| self.f64(x));
    }
    fn pairs(&mut self, v: &[Pair]) {
        self.len(v.len());
        for p in v {
            self.i32(p.code);
            self.str(&p.value);
        }
    }
    fn common(&mut self, c: &Common) {
        self.name(&c.layer);
        self.ostr(&c.linetype);
        self.oi32(c.color);
        self.oi32(c.true_color);
        self.oi32(c.lineweight);
        self.pairs(&c.extra);
    }
    fn fmt(&mut self, f: &TextFmt) {
        self.u8(f.mtext as u8);
        self.u8(f.halign);
        self.u8(f.valign);
        match f.align_off {
            Some(p) => {
                self.u8(1);
                self.p(p);
            }
            None => self.u8(0),
        }
        self.ostr(&f.style);
        self.f64(f.width_factor);
        self.f64(f.oblique);
        self.u8(f.attach);
        self.f64(f.box_width);
        self.f64(f.line_spacing);
        self.ostr(&f.raw);
    }
    fn attdef(&mut self, a: &AttDef) {
        self.common(&a.common);
        self.str(&a.tag);
        self.str(&a.prompt);
        self.str(&a.default);
        self.pt3(&a.pos);
        self.f64(a.height);
        self.i32(a.flags);
    }
    fn insert(&mut self, i: &Insert) {
        self.common(&i.common);
        self.str(&i.block);
        self.pt3(&i.pos);
        self.pt3(&i.scale);
        self.f64(i.rotation);
        self.len(i.attribs.len());
        for a in &i.attribs {
            self.common(&a.common);
            self.str(&a.tag);
            self.str(&a.value);
            self.pt3(&a.pos);
            self.f64(a.height);
            self.i32(a.flags);
        }
    }
    fn entity(&mut self, e: &Entity) {
        match e {
            Entity::Line(l) => {
                self.u8(0);
                self.common(&l.common);
                self.pt3(&l.start);
                self.pt3(&l.end);
            }
            Entity::LwPolyline(l) => {
                self.u8(1);
                self.common(&l.common);
                self.i32(l.flags);
                self.len(l.vertices.len());
                for v in &l.vertices {
                    for x in [v.x, v.y, v.bulge, v.start_width, v.end_width] {
                        self.f64(x);
                    }
                }
            }
            Entity::Circle(c) => {
                self.u8(2);
                self.common(&c.common);
                self.pt3(&c.center);
                self.f64(c.radius);
            }
            Entity::Arc(a) => {
                self.u8(3);
                self.common(&a.common);
                self.pt3(&a.center);
                self.f64(a.radius);
                self.f64(a.start_angle);
                self.f64(a.end_angle);
            }
            Entity::Text(t) => {
                self.u8(4);
                self.common(&t.common);
                self.pt3(&t.pos);
                self.f64(t.height);
                self.str(&t.text);
                self.f64(t.rotation);
                self.ostr(&t.style);
            }
            Entity::MText(t) => {
                self.u8(5);
                self.common(&t.common);
                self.pt3(&t.pos);
                self.f64(t.height);
                self.f64(t.width);
                self.str(&t.text);
                self.f64(t.rotation);
                self.ostr(&t.style);
                self.oi32(t.attach);
            }
            Entity::Insert(i) => {
                self.u8(6);
                self.insert(i);
            }
            Entity::AttDef(a) => {
                self.u8(7);
                self.attdef(a);
            }
            Entity::Unknown(v) => {
                self.u8(8);
                self.pairs(v);
            }
        }
    }
    fn style(&mut self, s: &TextStyle) {
        self.str(&s.name);
        self.i32(s.flags);
        self.f64(s.height);
        self.f64(s.width_factor);
        self.f64(s.oblique);
        self.str(&s.font);
        self.str(&s.big_font);
        self.pairs(&s.extra);
    }
    fn blocks(&mut self, b: &UBlocks) {
        let s = &*b.src;
        self.len(s.defs.len());
        for d in &s.defs {
            self.str(&d.name);
            self.str(&d.layer);
            self.i32(d.flags);
            self.pt3(&d.base);
            self.len(d.attdefs.len());
            d.attdefs.iter().for_each(|a| self.attdef(a));
            self.len(d.entities.len());
            d.entities.iter().for_each(|e| self.entity(e));
            self.pairs(&d.extra);
        }
        self.len(s.header.len());
        for h in &s.header {
            self.str(&h.name);
            self.pairs(&h.values);
        }
        self.len(s.layers.len());
        for l in &s.layers {
            self.str(&l.name);
            self.i32(l.flags);
            self.i32(l.color);
            self.str(&l.linetype);
            self.pairs(&l.extra);
        }
        self.len(s.ltypes.len());
        for l in &s.ltypes {
            self.str(&l.name);
            self.str(&l.description);
            self.i32(l.flags);
            self.f64(l.pattern_length);
            self.f64s(&l.pattern);
            self.pairs(&l.extra);
        }
        self.len(s.styles.len());
        s.styles.iter().for_each(|t| self.style(t));
        self.len(b.refs.len());
        for r in &b.refs {
            self.insert(&r.insert);
            self.p(r.base0);
            self.f64(r.rot0);
            self.p(r.pos);
            self.f64(r.rotation);
        }
    }
}

struct R<'a> {
    b: &'a [u8],
    at: usize,
    table: Vec<String>,
}

fn bad(what: &str) -> String {
    format!("damaged drawing record: {what}")
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len());
        let end = end.ok_or_else(|| bad("truncated"))?;
        let b: &'a [u8] = self.b;
        let s = &b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    /// A count, checked against what is left so a bad one cannot reserve huge memory.
    fn len(&mut self, min_item: usize) -> Result<usize, String> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_item.max(1)) > self.b.len() - self.at {
            return Err(bad("count past the end"));
        }
        Ok(n)
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn p(&mut self) -> Result<P, String> {
        Ok((self.f64()?, self.f64()?))
    }
    fn pts(&mut self) -> Result<Vec<P>, String> {
        let n = self.len(16)?;
        (0..n).map(|_| self.p()).collect()
    }
    fn str(&mut self) -> Result<String, String> {
        let n = self.len(1)?;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| bad("text"))
    }
    fn rgb(&mut self) -> Result<Option<[u8; 3]>, String> {
        Ok(match self.u8()? {
            0 => None,
            _ => {
                let c = self.take(3)?;
                Some([c[0], c[1], c[2]])
            }
        })
    }
    fn name(&mut self) -> Result<String, String> {
        let k = self.u32()? as usize;
        self.table.get(k).cloned().ok_or_else(|| bad("name index"))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn ostr(&mut self) -> Result<Option<String>, String> {
        Ok(match self.u8()? {
            0 => None,
            _ => Some(self.str()?),
        })
    }
    fn oi32(&mut self) -> Result<Option<i32>, String> {
        Ok(match self.u8()? {
            0 => None,
            _ => Some(self.i32()?),
        })
    }
    fn pt3(&mut self) -> Result<[f64; 3], String> {
        Ok([self.f64()?, self.f64()?, self.f64()?])
    }
    fn f64s(&mut self) -> Result<Vec<f64>, String> {
        let n = self.len(8)?;
        (0..n).map(|_| self.f64()).collect()
    }
    fn pairs(&mut self) -> Result<Vec<Pair>, String> {
        let n = self.len(8)?;
        (0..n)
            .map(|_| -> Result<Pair, String> {
                let code = self.i32()?;
                Ok(Pair {
                    code,
                    value: self.str()?,
                })
            })
            .collect()
    }
    fn common(&mut self) -> Result<Common, String> {
        Ok(Common {
            layer: self.name()?,
            linetype: self.ostr()?,
            color: self.oi32()?,
            true_color: self.oi32()?,
            lineweight: self.oi32()?,
            extra: self.pairs()?,
        })
    }
    fn fmt(&mut self) -> Result<TextFmt, String> {
        let mtext = self.u8()? != 0;
        let halign = self.u8()?;
        let valign = self.u8()?;
        let align_off = match self.u8()? {
            0 => None,
            _ => Some(self.p()?),
        };
        Ok(TextFmt {
            mtext,
            halign,
            valign,
            align_off,
            style: self.ostr()?,
            width_factor: self.f64()?,
            oblique: self.f64()?,
            attach: self.u8()?,
            box_width: self.f64()?,
            line_spacing: self.f64()?,
            raw: self.ostr()?,
        })
    }
    fn attdef(&mut self) -> Result<AttDef, String> {
        Ok(AttDef {
            common: self.common()?,
            tag: self.str()?,
            prompt: self.str()?,
            default: self.str()?,
            pos: self.pt3()?,
            height: self.f64()?,
            flags: self.i32()?,
        })
    }
    fn insert(&mut self) -> Result<Insert, String> {
        let common = self.common()?;
        let block = self.str()?;
        let (pos, scale, rotation) = (self.pt3()?, self.pt3()?, self.f64()?);
        let n = self.len(56)?;
        let mut attribs = Vec::with_capacity(n);
        for _ in 0..n {
            attribs.push(Attrib {
                common: self.common()?,
                tag: self.str()?,
                value: self.str()?,
                pos: self.pt3()?,
                height: self.f64()?,
                flags: self.i32()?,
            });
        }
        Ok(Insert {
            common,
            block,
            pos,
            scale,
            rotation,
            attribs,
        })
    }
    fn entity(&mut self) -> Result<Entity, String> {
        Ok(match self.u8()? {
            0 => Entity::Line(dx::Line {
                common: self.common()?,
                start: self.pt3()?,
                end: self.pt3()?,
            }),
            1 => {
                let common = self.common()?;
                let flags = self.i32()?;
                let n = self.len(40)?;
                let mut vertices = Vec::with_capacity(n);
                for _ in 0..n {
                    vertices.push(LwVertex {
                        x: self.f64()?,
                        y: self.f64()?,
                        bulge: self.f64()?,
                        start_width: self.f64()?,
                        end_width: self.f64()?,
                    });
                }
                Entity::LwPolyline(dx::LwPolyline {
                    common,
                    flags,
                    vertices,
                })
            }
            2 => Entity::Circle(dx::Circle {
                common: self.common()?,
                center: self.pt3()?,
                radius: self.f64()?,
            }),
            3 => Entity::Arc(dx::Arc {
                common: self.common()?,
                center: self.pt3()?,
                radius: self.f64()?,
                start_angle: self.f64()?,
                end_angle: self.f64()?,
            }),
            4 => Entity::Text(dx::Text {
                common: self.common()?,
                pos: self.pt3()?,
                height: self.f64()?,
                text: self.str()?,
                rotation: self.f64()?,
                style: self.ostr()?,
            }),
            5 => Entity::MText(dx::MText {
                common: self.common()?,
                pos: self.pt3()?,
                height: self.f64()?,
                width: self.f64()?,
                text: self.str()?,
                rotation: self.f64()?,
                style: self.ostr()?,
                attach: self.oi32()?,
            }),
            6 => Entity::Insert(self.insert()?),
            7 => Entity::AttDef(self.attdef()?),
            8 => Entity::Unknown(self.pairs()?),
            _ => return Err(bad("entity kind")),
        })
    }
    fn style(&mut self) -> Result<TextStyle, String> {
        Ok(TextStyle {
            name: self.str()?,
            flags: self.i32()?,
            height: self.f64()?,
            width_factor: self.f64()?,
            oblique: self.f64()?,
            font: self.str()?,
            big_font: self.str()?,
            extra: self.pairs()?,
        })
    }
    fn blocks(&mut self) -> Result<UBlocks, String> {
        let mut s = BlockSrc::default();
        let n = self.len(41)?;
        for _ in 0..n {
            let name = self.str()?;
            let layer = self.str()?;
            let flags = self.i32()?;
            let base = self.pt3()?;
            let k = self.len(30)?;
            let attdefs = (0..k).map(|_| self.attdef()).collect::<Result<_, _>>()?;
            let k = self.len(1)?;
            let entities = (0..k).map(|_| self.entity()).collect::<Result<_, _>>()?;
            s.defs.push(Block {
                name,
                layer,
                flags,
                base,
                attdefs,
                entities,
                extra: self.pairs()?,
            });
        }
        let n = self.len(8)?;
        for _ in 0..n {
            let name = self.str()?;
            s.header.push(HeaderVar {
                name,
                values: self.pairs()?,
            });
        }
        let n = self.len(20)?;
        for _ in 0..n {
            s.layers.push(Layer {
                name: self.str()?,
                flags: self.i32()?,
                color: self.i32()?,
                linetype: self.str()?,
                extra: self.pairs()?,
            });
        }
        let n = self.len(28)?;
        for _ in 0..n {
            s.ltypes.push(LineType {
                name: self.str()?,
                description: self.str()?,
                flags: self.i32()?,
                pattern_length: self.f64()?,
                pattern: self.f64s()?,
                extra: self.pairs()?,
            });
        }
        let n = self.len(44)?;
        for _ in 0..n {
            s.styles.push(self.style()?);
        }
        let n = self.len(100)?;
        let mut refs = Vec::with_capacity(n);
        for _ in 0..n {
            refs.push(URef {
                insert: self.insert()?,
                base0: self.p()?,
                rot0: self.f64()?,
                pos: self.p()?,
                rotation: self.f64()?,
            });
        }
        Ok(UBlocks {
            src: std::sync::Arc::new(s),
            refs,
        })
    }
}

fn body(u: &Underlay, w: &mut W) {
    w.len(u.lines.len());
    for l in &u.lines {
        w.name(&l.layer);
        w.p(l.a);
        w.p(l.b);
        w.u8(l.dashed as u8);
        w.u32(l.pen);
    }
    w.len(u.arcs.len());
    for a in &u.arcs {
        w.name(&a.layer);
        w.p(a.c);
        w.f64(a.r);
        w.f64(a.a0);
        w.f64(a.a1);
        w.u32(a.pen);
    }
    w.len(u.texts.len());
    for t in &u.texts {
        w.name(&t.layer);
        w.p(t.pos);
        w.f64(t.height);
        w.f64(t.rotation);
        w.str(&t.text);
        w.u32(t.pen);
        w.fmt(&t.fmt);
    }
    w.len(u.block_hints.len());
    for h in &u.block_hints {
        w.name(&h.name);
        w.name(&h.layer);
        w.u8(matches!(h.kind, BlockKind::Window) as u8);
        w.p(h.pos);
        w.f64(h.rotation);
        w.p(h.size);
    }
    w.len(u.pens.len());
    for p in &u.pens {
        w.rgb(p.color);
        w.f32(p.weight_mm);
        w.len(p.dash.len());
        p.dash.iter().for_each(|&d| w.f32(d));
    }
    w.len(u.closed.len());
    for c in &u.closed {
        w.name(&c.layer);
        w.pts(&c.pts);
        match c.circle {
            Some(r) => {
                w.u8(1);
                w.f64(r);
            }
            None => w.u8(0),
        }
    }
    w.len(u.fills.len());
    for f in &u.fills {
        w.name(&f.layer);
        w.len(f.loops.len());
        f.loops.iter().for_each(|l| w.pts(l));
        w.u8(f.solid as u8);
        w.str(&f.pattern);
        w.len(f.segs.len());
        for &(a, b) in &f.segs {
            w.p(a);
            w.p(b);
        }
        w.u32(f.pen);
    }
    w.len(u.inserts.len());
    for i in &u.inserts {
        w.name(&i.name);
        w.name(&i.layer);
        w.p(i.pos);
        w.f64(i.rotation);
        w.p(i.size);
        w.p(i.center);
        w.len(i.attribs.len());
        for (k, v) in &i.attribs {
            w.str(k);
            w.str(v);
        }
    }
    w.len(u.layers.len());
    for l in &u.layers {
        w.name(&l.name);
        w.rgb(l.color);
        w.u8(l.off as u8 | (l.frozen as u8) << 1);
        w.str(&l.linetype);
        w.f32(l.weight_mm);
    }
    w.len(u.layouts.len());
    u.layouts.iter().for_each(|s| w.str(s));
    w.u32(u.paper_skipped as u32);
    w.len(u.unknown.len());
    for (k, n) in &u.unknown {
        w.str(k);
        w.u32(*n as u32);
    }
    w.blocks(&u.blocks);
    w.len(u.text_styles.len());
    u.text_styles.iter().for_each(|t| w.style(t));
}

fn read_body(r: &mut R) -> Result<Underlay, String> {
    let mut u = Underlay::default();
    let n = r.len(41)?;
    u.lines.reserve(n);
    for _ in 0..n {
        u.lines.push(ULine {
            layer: r.name()?,
            a: r.p()?,
            b: r.p()?,
            dashed: r.u8()? != 0,
            pen: r.u32()?,
        });
    }
    let n = r.len(48)?;
    u.arcs.reserve(n);
    for _ in 0..n {
        u.arcs.push(UArc {
            layer: r.name()?,
            c: r.p()?,
            r: r.f64()?,
            a0: r.f64()?,
            a1: r.f64()?,
            pen: r.u32()?,
        });
    }
    let n = r.len(50)?;
    for _ in 0..n {
        u.texts.push(UText {
            layer: r.name()?,
            pos: r.p()?,
            height: r.f64()?,
            rotation: r.f64()?,
            text: r.str()?,
            pen: r.u32()?,
            fmt: r.fmt()?,
        });
    }
    let n = r.len(49)?;
    for _ in 0..n {
        u.block_hints.push(UBlockHint {
            name: r.name()?,
            layer: r.name()?,
            kind: if r.u8()? != 0 {
                BlockKind::Window
            } else {
                BlockKind::Door
            },
            pos: r.p()?,
            rotation: r.f64()?,
            size: r.p()?,
        });
    }
    let n = r.len(9)?;
    for _ in 0..n {
        let color = r.rgb()?;
        let weight_mm = r.f32()?;
        let k = r.len(4)?;
        let dash = (0..k).map(|_| r.f32()).collect::<Result<_, _>>()?;
        u.pens.push(Pen {
            color,
            weight_mm,
            dash,
        });
    }
    let n = r.len(9)?;
    for _ in 0..n {
        let layer = r.name()?;
        let pts = r.pts()?;
        let circle = match r.u8()? {
            0 => None,
            _ => Some(r.f64()?),
        };
        u.closed.push(UPoly { layer, pts, circle });
    }
    let n = r.len(17)?;
    for _ in 0..n {
        let layer = r.name()?;
        let k = r.len(4)?;
        let loops = (0..k).map(|_| r.pts()).collect::<Result<_, _>>()?;
        let solid = r.u8()? != 0;
        let pattern = r.str()?;
        let k = r.len(32)?;
        let segs = (0..k)
            .map(|_| Ok((r.p()?, r.p()?)))
            .collect::<Result<_, String>>()?;
        u.fills.push(UFill {
            layer,
            loops,
            solid,
            pattern,
            segs,
            pen: r.u32()?,
        });
    }
    let n = r.len(68)?;
    for _ in 0..n {
        let name = r.name()?;
        let layer = r.name()?;
        let (pos, rotation, size, center) = (r.p()?, r.f64()?, r.p()?, r.p()?);
        let k = r.len(8)?;
        let attribs = (0..k)
            .map(|_| Ok((r.str()?, r.str()?)))
            .collect::<Result<_, String>>()?;
        u.inserts.push(UInsert {
            name,
            layer,
            pos,
            rotation,
            size,
            center,
            attribs,
        });
    }
    let n = r.len(14)?;
    for _ in 0..n {
        let name = r.name()?;
        let color = r.rgb()?;
        let flags = r.u8()?;
        u.layers.push(ULayer {
            name,
            color,
            off: flags & 1 != 0,
            frozen: flags & 2 != 0,
            linetype: r.str()?,
            weight_mm: r.f32()?,
        });
    }
    let n = r.len(4)?;
    u.layouts = (0..n).map(|_| r.str()).collect::<Result<_, _>>()?;
    u.paper_skipped = r.u32()? as usize;
    let n = r.len(8)?;
    let mut unknown = BTreeMap::new();
    for _ in 0..n {
        let k = r.str()?;
        unknown.insert(k, r.u32()? as usize);
    }
    u.unknown = unknown;
    u.blocks = r.blocks()?;
    let n = r.len(44)?;
    for _ in 0..n {
        u.text_styles.push(r.style()?);
    }
    Ok(u)
}

/// Encoded and deflated record of `u`.
pub fn encode(u: &Underlay) -> Vec<u8> {
    let mut w = W {
        b: Vec::with_capacity(64 + u.lines.len() * 41 + u.arcs.len() * 48),
        names: HashMap::new(),
        table: vec![],
    };
    body(u, &mut w);
    let mut raw = Vec::with_capacity(w.b.len() + 1024);
    raw.extend_from_slice(MAGIC);
    raw.extend_from_slice(&FORMAT.to_le_bytes());
    raw.extend_from_slice(&(w.table.len() as u32).to_le_bytes());
    for s in &w.table {
        raw.extend_from_slice(&(s.len() as u32).to_le_bytes());
        raw.extend_from_slice(s.as_bytes());
    }
    raw.extend_from_slice(&w.b);
    // level 3: big drawings save fast; f64 coordinates gain little from harder levels
    miniz_oxide::deflate::compress_to_vec(&raw, 3)
}

/// Inverse of [`encode`]; a damaged or newer record is an error, never a partial drawing.
pub fn decode(blob: &[u8]) -> Result<Underlay, String> {
    let raw = miniz_oxide::inflate::decompress_to_vec_with_limit(blob, MAX_RAW)
        .map_err(|_| bad("not deflated"))?;
    let mut r = R {
        b: &raw,
        at: 0,
        table: vec![],
    };
    if r.take(4)? != MAGIC {
        return Err(bad("signature"));
    }
    let v = r.u32()?;
    if v != FORMAT {
        return Err(format!("drawing record format {v} is newer than this version"));
    }
    let n = r.len(4)?;
    for _ in 0..n {
        let s = r.str()?;
        r.table.push(s);
    }
    let u = read_body(&mut r)?;
    if r.at != raw.len() {
        return Err(bad("trailing bytes"));
    }
    Ok(u)
}

/// Primitives a drawing holds (what a reopened project must show again).
pub fn entity_count(u: &Underlay) -> usize {
    u.lines.len() + u.arcs.len() + u.texts.len() + u.closed.len() + u.fills.len() + u.inserts.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawing_model::fixture::sample_plan;

    fn big(n: usize) -> Underlay {
        let mut u = Underlay::default();
        for k in 0..n {
            let x = k as f64 * 0.137;
            let layer = format!("L{}", k % 40);
            match k % 4 {
                0 | 1 => u.lines.push(ULine {
                    layer,
                    a: (x, 1.0 / (k + 1) as f64),
                    b: (x + 0.5, -x),
                    dashed: k % 7 == 0,
                    pen: (k % 5) as u32,
                }),
                2 => u.arcs.push(UArc {
                    layer,
                    c: (x, x * 0.5),
                    r: 0.3,
                    a0: 0.1,
                    a1: 2.0,
                    pen: 1,
                }),
                _ => u.texts.push(UText {
                    layer,
                    pos: (x, 2.0),
                    height: 0.25,
                    rotation: 0.0,
                    text: format!("T{k}"),
                    pen: 0,
                    fmt: TextFmt::default(),
                }),
            }
        }
        u
    }

    #[test]
    fn sample_round_trip_is_exact() {
        let (u, _) = Underlay::from_dxf_auto(&sample_plan(0));
        assert!(entity_count(&u) > 0);
        assert_eq!(decode(&encode(&u)).unwrap(), u);
        assert_eq!(decode(&encode(&Underlay::default())).unwrap(), Underlay::default());
    }

    #[test]
    fn every_field_survives() {
        let mut u = big(12);
        u.closed.push(UPoly {
            layer: "R".into(),
            pts: vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
            circle: Some(0.5),
        });
        u.fills.push(UFill {
            layer: "H".into(),
            loops: vec![vec![(0.0, 0.0), (2.0, 0.0), (2.0, 2.0)], vec![]],
            solid: true,
            pattern: "ANSI31".into(),
            segs: vec![((0.0, 0.0), (1.0, 1.0))],
            pen: 2,
        });
        u.inserts.push(UInsert {
            name: "D1".into(),
            layer: "A".into(),
            pos: (1.0, 2.0),
            rotation: 0.5,
            size: (0.9, 0.2),
            center: (1.4, 2.1),
            attribs: vec![("TAG".into(), "v".into())],
        });
        u.block_hints.push(UBlockHint {
            name: "W1".into(),
            layer: "A".into(),
            kind: BlockKind::Window,
            pos: (3.0, 4.0),
            rotation: f64::MIN_POSITIVE,
            size: (1.2, 0.3),
        });
        u.pens.push(Pen {
            color: Some([1, 2, 3]),
            weight_mm: 0.35,
            dash: vec![0.5, -0.25],
        });
        u.layers.push(ULayer {
            name: "A".into(),
            color: None,
            off: true,
            frozen: true,
            linetype: "DASHED".into(),
            weight_mm: 0.18,
        });
        u.layouts = vec!["Model".into(), "Sheet 1".into()];
        u.paper_skipped = 7;
        u.unknown.insert("MESH".into(), 3);
        u.texts[0].fmt = TextFmt {
            mtext: true,
            halign: 4,
            valign: 2,
            align_off: Some((0.5, -0.25)),
            style: Some("ISO".into()),
            width_factor: 0.8,
            oblique: 15.0,
            attach: 5,
            box_width: 3.5,
            line_spacing: 1.2,
            raw: Some("A\\PB".into()),
        };
        let st = TextStyle {
            name: "ISO".into(),
            flags: 0,
            height: 0.0,
            width_factor: 0.8,
            oblique: 0.0,
            font: "iso.shx".into(),
            big_font: String::new(),
            extra: vec![Pair::new(1001, "APP")],
        };
        u.text_styles.push(st.clone());
        let common = Common {
            layer: "B".into(),
            linetype: Some("DASHED".into()),
            color: Some(3),
            true_color: Some(0x10_20_30),
            lineweight: Some(-1),
            extra: vec![Pair::new(1000, "x")],
        };
        let attdef = AttDef {
            common: common.clone(),
            tag: "T".into(),
            prompt: "P".into(),
            default: "D".into(),
            pos: [1.0, 2.0, 0.0],
            height: 2.5,
            flags: 8,
        };
        let insert = Insert {
            common: common.clone(),
            block: "BL".into(),
            pos: [1.0, 2.0, 0.0],
            scale: [-1.0, 1.0, 1.0],
            rotation: 30.0,
            attribs: vec![Attrib {
                common: common.clone(),
                tag: "T".into(),
                value: "v".into(),
                pos: [1.0, 2.0, 0.0],
                height: 2.5,
                flags: 0,
            }],
        };
        let entities = vec![
            Entity::Line(dx::Line {
                common: common.clone(),
                start: [0.0, 0.0, 0.0],
                end: [1.0, 1.0, 0.0],
            }),
            Entity::LwPolyline(dx::LwPolyline {
                common: common.clone(),
                flags: 1,
                vertices: vec![LwVertex {
                    x: 1.0,
                    y: 2.0,
                    bulge: 0.5,
                    start_width: 0.1,
                    end_width: 0.2,
                }],
            }),
            Entity::Circle(dx::Circle {
                common: common.clone(),
                center: [1.0, 1.0, 0.0],
                radius: 2.0,
            }),
            Entity::Arc(dx::Arc {
                common: common.clone(),
                center: [1.0, 1.0, 0.0],
                radius: 2.0,
                start_angle: 10.0,
                end_angle: 200.0,
            }),
            Entity::Text(dx::Text {
                common: common.clone(),
                pos: [0.0, 1.0, 0.0],
                height: 2.0,
                text: "t".into(),
                rotation: 5.0,
                style: Some("ISO".into()),
            }),
            Entity::MText(dx::MText {
                common: common.clone(),
                pos: [0.0, 1.0, 0.0],
                height: 2.0,
                width: 10.0,
                text: "m".into(),
                rotation: 0.0,
                style: None,
                attach: Some(7),
            }),
            Entity::Insert(insert.clone()),
            Entity::AttDef(attdef.clone()),
            Entity::Unknown(vec![Pair::new(0, "SPLINE"), Pair::new(8, "0")]),
        ];
        let src = BlockSrc {
            defs: vec![Block {
                name: "BL".into(),
                layer: "0".into(),
                flags: 2,
                base: [0.5, 0.5, 0.0],
                attdefs: vec![attdef],
                entities,
                extra: vec![Pair::new(4, "d")],
            }],
            header: vec![HeaderVar {
                name: "$LTSCALE".into(),
                values: vec![Pair::new(40, "2")],
            }],
            layers: vec![Layer {
                name: "B".into(),
                flags: 0,
                color: -7,
                linetype: "DASHED".into(),
                extra: vec![],
            }],
            ltypes: vec![LineType {
                name: "DASHED".into(),
                description: "- -".into(),
                flags: 0,
                pattern_length: 1.5,
                pattern: vec![1.0, -0.5],
                extra: vec![],
            }],
            styles: vec![st],
        };
        u.blocks = UBlocks {
            src: std::sync::Arc::new(src),
            refs: vec![URef {
                insert,
                base0: (1.0, 2.0),
                rot0: 0.5,
                pos: (3.0, 4.0),
                rotation: 0.7,
            }],
        };
        assert_eq!(decode(&encode(&u)).unwrap(), u);
    }

    #[test]
    fn damage_is_an_error_not_a_partial_drawing() {
        let blob = encode(&big(500));
        assert!(decode(&blob[..blob.len() / 2]).is_err());
        assert!(decode(b"not a record").is_err());
        let raw = miniz_oxide::deflate::compress_to_vec(b"DDCU\x09\0\0\0", 3);
        assert!(decode(&raw).unwrap_err().contains("newer"));
    }

    /// 110k primitives: compact and fast both ways.
    #[test]
    fn big_drawing_is_compact_and_quick() {
        let u = big(110_000);
        let t = web_time::Instant::now();
        let blob = encode(&u);
        let back = decode(&blob).unwrap();
        let took = t.elapsed();
        assert_eq!(entity_count(&back), 110_000);
        assert_eq!(back, u);
        assert!(blob.len() < 110_000 * 48, "{} bytes", blob.len());
        let bound = if cfg!(debug_assertions) { 30.0 } else { 2.0 };
        assert!(took.as_secs_f64() < bound, "{took:?}");
    }
}
