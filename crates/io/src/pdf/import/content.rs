//! Content stream interpreter: graphics state, paths, text, XObject forms,
//! marked OCG content. Output coordinates are sheet mm (base matrix in `Gs::ctm`).

use std::collections::HashMap;
use std::rc::Rc;

use super::lexer::{Dict, Obj, Parser, Tok};
use super::objects::Document;
use super::{PageDrawing, PathSeg, Shape, SubPath, TextRun, P};

pub type M = [f64; 6];
pub const ID: M = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
const MAX_FORM_DEPTH: usize = 12;
const MAX_OPS: usize = 50_000_000;
const MAX_POINTS: usize = 20_000_000;
pub const DEFAULT_LAYER: &str = "PDF";

/// `a`, then `b` (PDF multiplication order: CTM' = M × CTM).
pub fn mul(a: &M, b: &M) -> M {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}
pub fn apply(m: &M, x: f64, y: f64) -> P {
    [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]]
}
fn vec_len(m: &M, x: f64, y: f64) -> f64 {
    (m[0] * x + m[2] * y).hypot(m[1] * x + m[3] * y)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cs {
    Gray,
    Rgb,
    Cmyk,
    /// Separation/Indexed and other single-component spaces: tint 1 = black.
    Tint,
    Other,
}

#[derive(Clone)]
struct Gs {
    ctm: M,
    stroke: [u8; 3],
    fill: [u8; 3],
    scs: Cs,
    fcs: Cs,
    lw: f64,
    dash: Vec<f64>,
    font: Option<Rc<Font>>,
    fs: f64,
    tc: f64,
    tw: f64,
    th: f64,
    tl: f64,
    rise: f64,
}

fn rgb(c: [f64; 3]) -> [u8; 3] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}
fn color(cs: Cs, v: &[f64]) -> Option<[u8; 3]> {
    Some(match (cs, v.len()) {
        (Cs::Tint, 1) => rgb([1.0 - v[0]; 3]),
        (_, 1) => rgb([v[0]; 3]),
        (_, 3) => rgb([v[0], v[1], v[2]]),
        (_, 4) => {
            let k = v[3];
            rgb([
                (1.0 - v[0]) * (1.0 - k),
                (1.0 - v[1]) * (1.0 - k),
                (1.0 - v[2]) * (1.0 - k),
            ])
        }
        _ => return None,
    })
}

/// Font: decoding codes to Unicode and widths for advancing along the line.
pub struct Font {
    two_byte: bool,
    /// Code ranges (length in bytes, low, high) from codespacerange.
    codespace: Vec<(usize, u32, u32)>,
    uni: HashMap<u32, String>,
    ranges: Vec<(u32, u32, Vec<u16>)>,
    simple: Option<[char; 256]>,
    widths: HashMap<u32, f64>,
    dw: f64,
}

impl Font {
    fn codes(&self, s: &[u8]) -> Vec<(u32, usize)> {
        let mut out = vec![];
        let mut i = 0;
        while i < s.len() {
            let mut len = if self.two_byte { 2 } else { 1 };
            if !self.codespace.is_empty() {
                for n in 1..=4 {
                    let Some(b) = s.get(i..i + n) else { break };
                    let c = b.iter().fold(0u32, |a, &x| a << 8 | x as u32);
                    if self
                        .codespace
                        .iter()
                        .any(|&(l, lo, hi)| l == n && (lo..=hi).contains(&c))
                    {
                        len = n;
                        break;
                    }
                }
            }
            let b = &s[i..(i + len).min(s.len())];
            out.push((b.iter().fold(0u32, |a, &x| a << 8 | x as u32), b.len()));
            i += len;
        }
        out
    }

    fn unicode(&self, code: u32, out: &mut String) {
        if let Some(s) = self.uni.get(&code) {
            out.push_str(s);
            return;
        }
        for (lo, hi, base) in &self.ranges {
            if (*lo..=*hi).contains(&code) {
                let mut v = base.clone();
                if let Some(last) = v.last_mut() {
                    *last = last.wrapping_add((code - lo) as u16);
                }
                out.extend(char::decode_utf16(v).map(|c| c.unwrap_or('\u{FFFD}')));
                return;
            }
        }
        match &self.simple {
            Some(t) if code < 256 => out.push(t[code as usize]),
            _ => {
                if let Some(c) = char::from_u32(code).filter(|_| !self.two_byte) {
                    out.push(c)
                } else {
                    out.push('\u{FFFD}')
                }
            }
        }
    }

    fn width(&self, code: u32) -> f64 {
        self.widths.get(&code).copied().unwrap_or(self.dw) / 1000.0
    }
}

fn winansi_table() -> [char; 256] {
    let mut t = ['\u{FFFD}'; 256];
    for (i, c) in t.iter_mut().enumerate() {
        if (0x20..0x7f).contains(&i) || i >= 0xa0 {
            *c = char::from_u32(i as u32).unwrap_or('\u{FFFD}');
        }
    }
    let hi = [
        (0x80, '€'),
        (0x82, '‚'),
        (0x84, '„'),
        (0x85, '…'),
        (0x91, '‘'),
        (0x92, '’'),
        (0x93, '“'),
        (0x94, '”'),
        (0x95, '•'),
        (0x96, '–'),
        (0x97, '—'),
    ];
    for (i, c) in hi {
        t[i] = c;
    }
    t
}

fn glyph_char(name: &[u8]) -> Option<char> {
    let s = std::str::from_utf8(name).ok()?;
    if s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() {
        return s.chars().next();
    }
    if let Some(h) = s.strip_prefix("uni").or_else(|| s.strip_prefix('u')) {
        if (4..=6).contains(&h.len()) {
            return u32::from_str_radix(h, 16).ok().and_then(char::from_u32);
        }
    }
    const DIG: [&str; 10] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    ];
    if let Some(d) = DIG.iter().position(|d| *d == s) {
        return char::from_digit(d as u32, 10);
    }
    Some(match s {
        "space" => ' ',
        "period" => '.',
        "comma" => ',',
        "hyphen" | "minus" => '-',
        "colon" => ':',
        "semicolon" => ';',
        "slash" => '/',
        "parenleft" => '(',
        "parenright" => ')',
        "degree" => '°',
        "plusminus" => '±',
        "multiply" => '×',
        "plus" => '+',
        "equal" => '=',
        "underscore" => '_',
        "percent" => '%',
        "numbersign" => '#',
        "quotesingle" => '\'',
        "quotedbl" => '"',
        _ => return None,
    })
}

/// ToUnicode parsing: codespacerange, bfchar, bfrange.
fn parse_cmap(f: &mut Font, data: &[u8]) {
    let mut p = Parser::new(data, 0, false);
    let mut stack: Vec<Obj> = vec![];
    let utf16 = |b: &[u8]| -> Vec<u16> {
        b.chunks(2)
            .map(|c| (c[0] as u16) << 8 | *c.get(1).unwrap_or(&0) as u16)
            .collect()
    };
    let code = |b: &[u8]| b.iter().take(4).fold(0u32, |a, &x| a << 8 | x as u32);
    let mut mode = 0;
    let mut guard = 0usize;
    loop {
        guard += 1;
        if guard > 2_000_000 {
            break;
        }
        let t = p.lx.next();
        match t {
            Tok::Eof => break,
            Tok::Keyword(k) => {
                match k {
                    b"begincodespacerange" => mode = 1,
                    b"beginbfchar" => mode = 2,
                    b"beginbfrange" => mode = 3,
                    b"endcodespacerange" => {
                        for pr in stack.chunks(2) {
                            if let [Obj::Str(a), Obj::Str(b)] = pr {
                                f.codespace.push((a.len().clamp(1, 4), code(a), code(b)));
                            }
                        }
                        mode = 0;
                    }
                    b"endbfchar" => {
                        for pr in stack.chunks(2) {
                            if let [Obj::Str(a), Obj::Str(b)] = pr {
                                let s: String = char::decode_utf16(utf16(b))
                                    .map(|c| c.unwrap_or('\u{FFFD}'))
                                    .collect();
                                f.uni.insert(code(a), s);
                            }
                        }
                        mode = 0;
                    }
                    b"endbfrange" => {
                        for tr in stack.chunks(3) {
                            match tr {
                                [Obj::Str(a), Obj::Str(b), Obj::Str(d)] => {
                                    let (lo, hi) = (code(a), code(b));
                                    if lo <= hi && !d.is_empty() {
                                        f.ranges.push((lo, hi, utf16(d)));
                                    }
                                }
                                [Obj::Str(a), Obj::Str(_), Obj::Array(arr)] => {
                                    let lo = code(a);
                                    for (i, d) in arr.iter().enumerate().take(65536) {
                                        if let Obj::Str(d) = d {
                                            let s: String = char::decode_utf16(utf16(d))
                                                .map(|c| c.unwrap_or('\u{FFFD}'))
                                                .collect();
                                            f.uni.insert(lo.wrapping_add(i as u32), s);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        mode = 0;
                    }
                    _ => {}
                }
                stack.clear();
            }
            t => {
                if let Ok(o) = p.obj_from(t, 0) {
                    if mode != 0 && stack.len() < 1_000_000 {
                        stack.push(o);
                    }
                }
            }
        }
    }
}

fn load_font(doc: &Document, fd: &Dict) -> Font {
    let sub = doc.get_in(fd, b"Subtype");
    let two_byte = sub.name() == Some(b"Type0");
    let mut f = Font {
        two_byte,
        codespace: vec![],
        uni: HashMap::new(),
        ranges: vec![],
        simple: None,
        widths: HashMap::new(),
        dw: if two_byte { 1000.0 } else { 0.0 },
    };
    if let Obj::Stream(d, raw) = &*doc.get_in(fd, b"ToUnicode") {
        if let Ok(data) = doc.decode_stream(d, raw) {
            parse_cmap(&mut f, &data);
        }
    }
    if two_byte {
        let desc = doc.get_in(fd, b"DescendantFonts");
        let df = desc.array().and_then(|a| a.first()).map(|o| doc.resolve(o));
        if let Some(df) = df.as_ref().and_then(|o| o.dict()) {
            if let Some(dw) = doc.get_in(df, b"DW").num() {
                f.dw = dw;
            }
            if let Some(w) = doc.get_in(df, b"W").array() {
                let mut i = 0;
                while i + 1 < w.len() && f.widths.len() < 200_000 {
                    let c0 = doc.resolve(&w[i]).int().unwrap_or(0).max(0) as u32;
                    match &*doc.resolve(&w[i + 1]) {
                        Obj::Array(ws) => {
                            for (k, x) in ws.iter().enumerate() {
                                if let Some(v) = doc.resolve(x).num() {
                                    f.widths.insert(c0.saturating_add(k as u32), v);
                                }
                            }
                            i += 2;
                        }
                        o => {
                            let c1 = o.int().unwrap_or(-1);
                            let v = w.get(i + 2).map(|x| doc.resolve(x).num().unwrap_or(0.0));
                            if let Some(v) = v {
                                let c1 = c1.clamp(-1, c0 as i64 + 65535);
                                let mut c = c0 as i64;
                                while c <= c1 {
                                    f.widths.insert(c as u32, v);
                                    c += 1;
                                }
                            }
                            i += 3;
                        }
                    }
                }
            }
        }
    } else {
        let first = doc
            .get_in(fd, b"FirstChar")
            .int()
            .unwrap_or(0)
            .clamp(0, 255) as u32;
        if let Some(w) = doc.get_in(fd, b"Widths").array() {
            for (k, x) in w.iter().enumerate().take(256) {
                if let Some(v) = doc.resolve(x).num() {
                    f.widths.insert(first + k as u32, v);
                }
            }
        }
        let mut t = winansi_table();
        let enc = doc.get_in(fd, b"Encoding");
        if let Some(ed) = enc.dict() {
            if let Some(diff) = doc.get_in(ed, b"Differences").array() {
                let mut c = 0usize;
                for o in diff {
                    match &*doc.resolve(o) {
                        Obj::Int(i) => c = (*i).clamp(0, 255) as usize,
                        Obj::Name(n) => {
                            if c < 256 {
                                if let Some(ch) = glyph_char(n) {
                                    t[c] = ch;
                                }
                            }
                            c += 1;
                        }
                        _ => {}
                    }
                }
            }
        }
        f.simple = Some(t);
    }
    f
}

pub fn text_of(b: &[u8]) -> String {
    if b.starts_with(&[0xFE, 0xFF]) {
        let u: Vec<u16> = b[2..]
            .chunks(2)
            .map(|c| (c[0] as u16) << 8 | *c.get(1).unwrap_or(&0) as u16)
            .collect();
        return char::decode_utf16(u)
            .map(|c| c.unwrap_or('\u{FFFD}'))
            .collect();
    }
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

pub struct Interp<'d> {
    doc: &'d Document,
    pub out: PageDrawing,
    layer_ix: HashMap<String, usize>,
    ops: usize,
    points: usize,
    fonts: HashMap<u32, Rc<Font>>,
}

struct Run {
    path: Vec<SubPath>,
    cur: Option<P>,
    tm: M,
    tlm: M,
    mc: Vec<Option<usize>>,
    /// Nested BDC beyond the stack limit: their EMCs are ignored.
    mc_over: usize,
    base_layer: Option<usize>,
}

impl<'d> Interp<'d> {
    pub fn new(doc: &'d Document, out: PageDrawing) -> Self {
        Self {
            doc,
            out,
            layer_ix: HashMap::new(),
            ops: 0,
            points: 0,
            fonts: HashMap::new(),
        }
    }

    fn layer(&mut self, name: String) -> usize {
        if let Some(&i) = self.layer_ix.get(&name) {
            return i;
        }
        let i = self.out.layers.len();
        self.out.layers.push(name.clone());
        self.layer_ix.insert(name, i);
        i
    }

    /// Layer name from an OCG or OCMD dictionary.
    fn ocg_layer(&mut self, o: &Obj) -> Option<usize> {
        let o = self.doc.resolve(o);
        let d = o.dict()?;
        let t = self.doc.get_in(d, b"Type");
        let ocg = if t.name() == Some(b"OCMD") {
            let g = self.doc.get_in(d, b"OCGs");
            match &*g {
                Obj::Array(a) => self.doc.resolve(a.first()?),
                Obj::Dict(_) => g.clone(),
                _ => return None,
            }
        } else {
            o.clone()
        };
        let n = self.doc.get_in(ocg.dict()?, b"Name");
        let name = match &*n {
            Obj::Str(s) => text_of(s),
            Obj::Name(s) => text_of(s),
            _ => return None,
        };
        let name = name.trim().to_string();
        Some(self.layer(if name.is_empty() {
            DEFAULT_LAYER.into()
        } else {
            name
        }))
    }

    fn cur_layer(&mut self, r: &Run) -> usize {
        match r.mc.iter().rev().find_map(|x| *x).or(r.base_layer) {
            Some(l) => l,
            None => self.layer(DEFAULT_LAYER.into()),
        }
    }

    fn res_entry(&self, res: &Obj, cat: &[u8], name: &[u8]) -> Option<(Option<u32>, Rc<Obj>)> {
        let rd = res.dict()?;
        let c = self.doc.get_in(rd, cat);
        let raw = c.dict()?.get(name)?;
        let n = match raw {
            Obj::Ref(n, _) => Some(*n),
            _ => None,
        };
        Some((n, self.doc.resolve(raw)))
    }

    fn cs_of(&self, res: &Obj, name: &[u8]) -> Cs {
        let by_name = |n: &[u8]| match n {
            b"DeviceGray" | b"G" | b"CalGray" => Some(Cs::Gray),
            b"DeviceRGB" | b"RGB" | b"CalRGB" | b"Lab" => Some(Cs::Rgb),
            b"DeviceCMYK" | b"CMYK" => Some(Cs::Cmyk),
            b"Separation" | b"Indexed" | b"I" => Some(Cs::Tint),
            b"Pattern" => Some(Cs::Other),
            _ => None,
        };
        if let Some(c) = by_name(name) {
            return c;
        }
        let Some((_, o)) = self.res_entry(res, b"ColorSpace", name) else {
            return Cs::Other;
        };
        match &*o {
            Obj::Name(n) => by_name(n).unwrap_or(Cs::Other),
            Obj::Array(a) => match a.first().and_then(Obj::name) {
                Some(b"ICCBased") => {
                    let s = a.get(1).map(|x| self.doc.resolve(x));
                    match s
                        .as_ref()
                        .and_then(|s| s.dict())
                        .and_then(|d| d.get(b"N"))
                        .and_then(Obj::int)
                    {
                        Some(1) => Cs::Gray,
                        Some(4) => Cs::Cmyk,
                        _ => Cs::Rgb,
                    }
                }
                Some(n) => by_name(n).unwrap_or(Cs::Other),
                None => Cs::Other,
            },
            _ => Cs::Other,
        }
    }

    fn font(&mut self, res: &Obj, name: &[u8]) -> Option<Rc<Font>> {
        let (num, o) = self.res_entry(res, b"Font", name)?;
        if let Some(f) = num.and_then(|n| self.fonts.get(&n)) {
            return Some(f.clone());
        }
        let f = Rc::new(load_font(self.doc, o.dict()?));
        if let Some(n) = num {
            self.fonts.insert(n, f.clone());
        }
        Some(f)
    }

    pub fn run_page(&mut self, content: &[u8], res: Rc<Obj>, base: M) {
        let gs = Gs {
            ctm: base,
            stroke: [0; 3],
            fill: [0; 3],
            scs: Cs::Gray,
            fcs: Cs::Gray,
            lw: 1.0,
            dash: vec![],
            font: None,
            fs: 0.0,
            tc: 0.0,
            tw: 0.0,
            th: 1.0,
            tl: 0.0,
            rise: 0.0,
        };
        self.run(content, &res, gs, None, 0);
    }

    fn budget(&mut self) -> bool {
        if self.ops > MAX_OPS || self.points > MAX_POINTS {
            self.out.truncated = true;
            return false;
        }
        true
    }

    fn run(
        &mut self,
        content: &[u8],
        res: &Rc<Obj>,
        gs0: Gs,
        base_layer: Option<usize>,
        depth: usize,
    ) {
        let mut stack: Vec<Gs> = vec![];
        let mut gs = gs0;
        let mut r = Run {
            path: vec![],
            cur: None,
            tm: ID,
            tlm: ID,
            mc: vec![],
            mc_over: 0,
            base_layer,
        };
        let mut p = Parser::new(content, 0, false);
        let mut args: Vec<Obj> = Vec::with_capacity(16);
        loop {
            let t = p.lx.next();
            let op = match t {
                Tok::Eof => break,
                Tok::Keyword(k) => k,
                Tok::Bad | Tok::ArrClose | Tok::DictClose => continue,
                t => {
                    match p.obj_from(t, 0) {
                        Ok(o) if args.len() < 4096 => args.push(o),
                        _ => {}
                    }
                    continue;
                }
            };
            self.ops += 1;
            if self.ops & 0xFFF == 0 && !self.budget() {
                return;
            }
            let n = |i: usize| args.get(i).and_then(Obj::num).unwrap_or(0.0);
            let nums = |a: &[Obj]| a.iter().filter_map(Obj::num).collect::<Vec<f64>>();
            match op {
                b"q" if stack.len() < 4096 => stack.push(gs.clone()),
                b"Q" => {
                    if let Some(g) = stack.pop() {
                        gs = g;
                    }
                }
                b"cm" if args.len() >= 6 => {
                    let m = [n(0), n(1), n(2), n(3), n(4), n(5)];
                    gs.ctm = mul(&m, &gs.ctm);
                }
                b"w" => gs.lw = n(0).abs(),
                b"d" => {
                    gs.dash = args
                        .first()
                        .and_then(Obj::array)
                        .map(nums)
                        .unwrap_or_default();
                    if gs.dash.iter().all(|v| *v <= 0.0) {
                        gs.dash.clear();
                    }
                }
                b"G" => (gs.scs, gs.stroke) = (Cs::Gray, rgb([n(0); 3])),
                b"g" => (gs.fcs, gs.fill) = (Cs::Gray, rgb([n(0); 3])),
                b"RG" => (gs.scs, gs.stroke) = (Cs::Rgb, rgb([n(0), n(1), n(2)])),
                b"rg" => (gs.fcs, gs.fill) = (Cs::Rgb, rgb([n(0), n(1), n(2)])),
                b"K" => {
                    gs.scs = Cs::Cmyk;
                    gs.stroke = color(Cs::Cmyk, &[n(0), n(1), n(2), n(3)]).unwrap_or([0; 3]);
                }
                b"k" => {
                    gs.fcs = Cs::Cmyk;
                    gs.fill = color(Cs::Cmyk, &[n(0), n(1), n(2), n(3)]).unwrap_or([0; 3]);
                }
                b"CS" | b"cs" => {
                    let cs = args
                        .first()
                        .and_then(Obj::name)
                        .map_or(Cs::Other, |nm| self.cs_of(res, nm));
                    if op == b"CS" {
                        gs.scs = cs;
                        gs.stroke = [0; 3];
                    } else {
                        gs.fcs = cs;
                        gs.fill = [0; 3];
                    }
                }
                b"SC" | b"SCN" | b"sc" | b"scn" => {
                    let v = nums(&args);
                    let stroke = op[0] == b'S';
                    let cs = if stroke { gs.scs } else { gs.fcs };
                    if let Some(c) = color(cs, &v) {
                        if stroke {
                            gs.stroke = c;
                        } else {
                            gs.fill = c;
                        }
                    }
                }
                b"m" if args.len() >= 2 => {
                    let pt = apply(&gs.ctm, n(0), n(1));
                    self.points += 1;
                    r.path.push(SubPath {
                        start: pt,
                        segs: vec![],
                        closed: false,
                    });
                    r.cur = Some(pt);
                }
                b"l" if args.len() >= 2 => {
                    let pt = apply(&gs.ctm, n(0), n(1));
                    self.seg(&mut r, PathSeg::Line(pt), pt);
                }
                b"c" | b"v" | b"y" => {
                    let need = if op == b"c" { 6 } else { 4 };
                    if args.len() >= need {
                        let Some(c0) = r.cur else {
                            args.clear();
                            continue;
                        };
                        let q = |i: usize| apply(&gs.ctm, n(i), n(i + 1));
                        let (c1, c2, e) = match op {
                            b"c" => (q(0), q(2), q(4)),
                            b"v" => (c0, q(0), q(2)),
                            _ => (q(0), q(2), q(2)),
                        };
                        self.seg(&mut r, PathSeg::Cubic(c1, c2, e), e);
                    }
                }
                b"h" => {
                    if let Some(s) = r.path.last_mut() {
                        s.closed = true;
                        r.cur = Some(s.start);
                    }
                }
                b"re" if args.len() >= 4 => {
                    let (x, y, w, h) = (n(0), n(1), n(2), n(3));
                    let c = |a: f64, b: f64| apply(&gs.ctm, a, b);
                    self.points += 4;
                    r.path.push(SubPath {
                        start: c(x, y),
                        segs: vec![
                            PathSeg::Line(c(x + w, y)),
                            PathSeg::Line(c(x + w, y + h)),
                            PathSeg::Line(c(x, y + h)),
                        ],
                        closed: true,
                    });
                    r.cur = Some(c(x, y));
                }
                b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"n" => {
                    if matches!(op, b"s" | b"b" | b"b*") {
                        if let Some(s) = r.path.last_mut() {
                            s.closed = true;
                        }
                    }
                    let stroke = matches!(op, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*");
                    let fill = matches!(op, b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*");
                    let subpaths = std::mem::take(&mut r.path);
                    r.cur = None;
                    if (stroke || fill) && !subpaths.is_empty() {
                        let sc = gs.ctm[0] * gs.ctm[3] - gs.ctm[1] * gs.ctm[2];
                        let k = sc.abs().sqrt();
                        let layer = self.cur_layer(&r);
                        self.out.shapes.push(Shape {
                            subpaths,
                            stroke,
                            fill,
                            rgb: if stroke { gs.stroke } else { gs.fill },
                            width_mm: gs.lw * k,
                            dash: gs.dash.iter().map(|v| v * k).collect(),
                            layer,
                        });
                    }
                }
                b"BT" => {
                    r.tm = ID;
                    r.tlm = ID;
                }
                b"Tf" if args.len() >= 2 => {
                    gs.font = args[0].name().and_then(|nm| self.font(res, nm));
                    gs.fs = n(1);
                }
                b"Tc" => gs.tc = n(0),
                b"Tw" => gs.tw = n(0),
                b"Tz" => gs.th = n(0) / 100.0,
                b"TL" => gs.tl = n(0),
                b"Ts" => gs.rise = n(0),
                b"Td" | b"TD" if args.len() >= 2 => {
                    if op == b"TD" {
                        gs.tl = -n(1);
                    }
                    r.tlm = mul(&[1.0, 0.0, 0.0, 1.0, n(0), n(1)], &r.tlm);
                    r.tm = r.tlm;
                }
                b"Tm" if args.len() >= 6 => {
                    r.tlm = [n(0), n(1), n(2), n(3), n(4), n(5)];
                    r.tm = r.tlm;
                }
                b"T*" => {
                    r.tlm = mul(&[1.0, 0.0, 0.0, 1.0, 0.0, -gs.tl], &r.tlm);
                    r.tm = r.tlm;
                }
                b"Tj" | b"'" | b"\"" | b"TJ" => {
                    if op == b"\"" && args.len() >= 3 {
                        gs.tw = n(0);
                        gs.tc = n(1);
                    }
                    if op != b"Tj" && op != b"TJ" {
                        r.tlm = mul(&[1.0, 0.0, 0.0, 1.0, 0.0, -gs.tl], &r.tlm);
                        r.tm = r.tlm;
                    }
                    let items: Vec<Obj> = match (op, args.last()) {
                        (b"TJ", Some(Obj::Array(a))) => a.clone(),
                        (_, Some(o @ Obj::Str(_))) => vec![o.clone()],
                        _ => vec![],
                    };
                    self.show(&mut r, &gs, &items);
                }
                b"Do" => {
                    if let Some(nm) = args.first().and_then(Obj::name) {
                        let nm = nm.to_vec();
                        self.form(&mut r, res, &gs, &nm, depth);
                    }
                }
                b"BDC" | b"BMC" => {
                    let mut l = None;
                    if op == b"BDC" && args.first().and_then(Obj::name) == Some(b"OC") {
                        let prop = match args.get(1) {
                            Some(Obj::Name(pn)) => {
                                self.res_entry(res, b"Properties", pn).map(|x| x.1)
                            }
                            Some(o @ Obj::Dict(_)) => Some(Rc::new(o.clone())),
                            _ => None,
                        };
                        l = prop.and_then(|o| self.ocg_layer(&o));
                    }
                    if r.mc.len() < 4096 {
                        r.mc.push(l);
                    } else {
                        r.mc_over += 1;
                    }
                }
                b"EMC" => {
                    if r.mc_over > 0 {
                        r.mc_over -= 1;
                    } else {
                        r.mc.pop();
                    }
                }
                b"BI" => skip_inline_image(&mut p),
                _ => {}
            }
            args.clear();
        }
    }

    fn seg(&mut self, r: &mut Run, s: PathSeg, end: P) {
        self.points += 1;
        if r.path.is_empty() || r.cur.is_none() {
            // A segment without "m": start the subpath at the current point or at the segment end.
            let st = r.cur.unwrap_or(end);
            r.path.push(SubPath {
                start: st,
                segs: vec![],
                closed: false,
            });
            if r.cur.is_none() {
                r.cur = Some(end);
                return;
            }
        }
        if let Some(sp) = r.path.last_mut() {
            if sp.closed {
                // After "h" a new segment starts a new subpath from the start of the previous one.
                let st = sp.start;
                r.path.push(SubPath {
                    start: st,
                    segs: vec![s],
                    closed: false,
                });
            } else {
                sp.segs.push(s);
            }
        }
        r.cur = Some(end);
    }

    fn show(&mut self, r: &mut Run, gs: &Gs, items: &[Obj]) {
        let Some(font) = gs.font.clone() else {
            return;
        };
        let trm = mul(&r.tm, &gs.ctm);
        let pos = apply(&trm, 0.0, gs.rise);
        let height = vec_len(&trm, 0.0, gs.fs);
        let rotation = (trm[1]).atan2(trm[0]).to_degrees();
        let mut s = String::new();
        for it in items {
            match it {
                Obj::Str(b) => {
                    for (code, len) in font.codes(b) {
                        font.unicode(code, &mut s);
                        let ws = if len == 1 && code == 32 { gs.tw } else { 0.0 };
                        let tx = (font.width(code) * gs.fs + gs.tc + ws) * gs.th;
                        r.tm = mul(&[1.0, 0.0, 0.0, 1.0, tx, 0.0], &r.tm);
                    }
                }
                o => {
                    if let Some(adj) = o.num() {
                        let tx = -adj / 1000.0 * gs.fs * gs.th;
                        r.tm = mul(&[1.0, 0.0, 0.0, 1.0, tx, 0.0], &r.tm);
                    }
                }
            }
        }
        let s: String = s.chars().filter(|c| *c != '\0').collect();
        if s.trim().is_empty() || !(pos[0].is_finite() && pos[1].is_finite() && height.is_finite())
        {
            return;
        }
        let layer = self.cur_layer(r);
        self.out.texts.push(TextRun {
            pos,
            height_mm: height,
            rotation_deg: rotation,
            string: s,
            rgb: gs.fill,
            layer,
        });
    }

    fn form(&mut self, r: &mut Run, res: &Rc<Obj>, gs: &Gs, name: &[u8], depth: usize) {
        if depth >= MAX_FORM_DEPTH {
            self.out.truncated = true;
            return;
        }
        let Some((_, o)) = self.res_entry(res, b"XObject", name) else {
            return;
        };
        let Obj::Stream(d, raw) = &*o else { return };
        if self.doc.get_in(d, b"Subtype").name() != Some(b"Form") {
            return;
        }
        let Ok(data) = self.doc.decode_stream(d, raw) else {
            return;
        };
        let mut g = gs.clone();
        if let Some(m) = self.doc.get_in(d, b"Matrix").array() {
            let v: Vec<f64> = m.iter().map(|x| x.num().unwrap_or(0.0)).collect();
            if v.len() == 6 {
                g.ctm = mul(&[v[0], v[1], v[2], v[3], v[4], v[5]], &g.ctm);
            }
        }
        let fres = self.doc.get_in(d, b"Resources");
        let fres = if fres.dict().is_some() {
            fres
        } else {
            res.clone()
        };
        let layer = match d.get(b"OC") {
            Some(oc) => self.ocg_layer(oc),
            None => None,
        }
        .or_else(|| r.mc.iter().rev().find_map(|x| *x))
        .or(r.base_layer);
        self.run(&data, &fres, g, layer, depth + 1);
    }
}

/// BI … ID <data> EI: the data is binary, look for "EI" between whitespace characters.
fn skip_inline_image(p: &mut Parser) {
    loop {
        match p.lx.next() {
            Tok::Eof => return,
            Tok::Keyword(b"ID") => break,
            _ => {}
        }
    }
    let b = p.lx.b;
    let mut i = p.lx.pos + 1;
    while i + 1 < b.len() {
        if b[i] == b'E'
            && b[i + 1] == b'I'
            && super::lexer::is_ws(b[i - 1])
            && b.get(i + 2).is_none_or(|c| super::lexer::is_ws(*c))
        {
            p.lx.pos = i + 2;
            return;
        }
        i += 1;
    }
    p.lx.pos = b.len();
}
