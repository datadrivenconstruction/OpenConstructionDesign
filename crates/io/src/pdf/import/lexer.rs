//! PDF lexer and object parser. Works on a byte slice and never panics:
//! broken tokens yield `Tok::Bad`, nesting is capped by `MAX_DEPTH`.

use std::rc::Rc;

pub const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    Array(Vec<Obj>),
    Dict(Dict),
    /// A dictionary and the raw (undecoded) data.
    Stream(Dict, Rc<[u8]>),
    Ref(u32, u16),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Dict(pub Vec<(Vec<u8>, Obj)>);

impl Dict {
    pub fn get(&self, k: &[u8]) -> Option<&Obj> {
        self.0.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v)
    }
}

impl Obj {
    pub fn num(&self) -> Option<f64> {
        match *self {
            Obj::Int(i) => Some(i as f64),
            Obj::Real(r) if r.is_finite() => Some(r),
            _ => None,
        }
    }
    pub fn int(&self) -> Option<i64> {
        match *self {
            Obj::Int(i) => Some(i),
            Obj::Real(r) if r.is_finite() => Some(r as i64),
            _ => None,
        }
    }
    pub fn name(&self) -> Option<&[u8]> {
        match self {
            Obj::Name(n) => Some(n),
            _ => None,
        }
    }
    pub fn dict(&self) -> Option<&Dict> {
        match self {
            Obj::Dict(d) | Obj::Stream(d, _) => Some(d),
            _ => None,
        }
    }
    pub fn array(&self) -> Option<&[Obj]> {
        match self {
            Obj::Array(a) => Some(a),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok<'a> {
    Int(i64),
    Real(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    ArrOpen,
    ArrClose,
    DictOpen,
    DictClose,
    Keyword(&'a [u8]),
    /// An unrecognised byte (e.g. a lone `}` or `>`).
    Bad,
    Eof,
}

pub fn is_ws(c: u8) -> bool {
    matches!(c, 0 | 9 | 10 | 12 | 13 | 32)
}
pub fn is_delim(c: u8) -> bool {
    matches!(
        c,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}
fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

pub struct Lexer<'a> {
    pub b: &'a [u8],
    pub pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(b: &'a [u8], pos: usize) -> Self {
        Self {
            b,
            pos: pos.min(b.len()),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    pub fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if is_ws(c) {
                self.pos += 1;
            } else if c == b'%' {
                while let Some(c) = self.peek() {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    pub fn next(&mut self) -> Tok<'a> {
        self.skip_ws();
        let Some(c) = self.peek() else {
            return Tok::Eof;
        };
        match c {
            b'[' => {
                self.pos += 1;
                Tok::ArrOpen
            }
            b']' => {
                self.pos += 1;
                Tok::ArrClose
            }
            b'<' if self.b.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                Tok::DictOpen
            }
            b'>' if self.b.get(self.pos + 1) == Some(&b'>') => {
                self.pos += 2;
                Tok::DictClose
            }
            b'<' => {
                self.pos += 1;
                Tok::Str(self.hex_string())
            }
            b'(' => {
                self.pos += 1;
                Tok::Str(self.lit_string())
            }
            b'/' => {
                self.pos += 1;
                Tok::Name(self.name())
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.number(),
            _ if is_delim(c) => {
                self.pos += 1;
                Tok::Bad
            }
            _ => {
                let s = self.pos;
                while let Some(c) = self.peek() {
                    if is_ws(c) || is_delim(c) {
                        break;
                    }
                    self.pos += 1;
                }
                Tok::Keyword(&self.b[s..self.pos])
            }
        }
    }

    fn number(&mut self) -> Tok<'a> {
        let s = self.pos;
        let mut neg = false;
        // Accept "--5" and "+-5" like common viewers do: the last sign wins.
        while let Some(c @ (b'+' | b'-')) = self.peek() {
            neg = c == b'-';
            self.pos += 1;
        }
        let mut int: i64 = 0;
        let mut overflow = false;
        let mut digits = 0;
        while let Some(c @ b'0'..=b'9') = self.peek() {
            match int
                .checked_mul(10)
                .and_then(|v| v.checked_add((c - b'0') as i64))
            {
                Some(v) => int = v,
                None => overflow = true,
            }
            digits += 1;
            self.pos += 1;
        }
        let real = self.peek() == Some(b'.');
        let (mut frac, mut fd) = (0i64, 0u32);
        if real {
            self.pos += 1;
            while let Some(c @ b'0'..=b'9') = self.peek() {
                if fd < 18 {
                    frac = frac * 10 + (c - b'0') as i64;
                    fd += 1;
                }
                digits += 1;
                self.pos += 1;
            }
        }
        let clean = self.peek().is_none_or(|c| is_ws(c) || is_delim(c));
        // Fast path: a mantissa < 2^53 and a divisor ≤ 1e22 give exact rounding.
        if real && clean && !overflow && fd <= 15 && digits > 0 {
            let p = 10i64.pow(fd);
            if let Some(m) = int.checked_mul(p).and_then(|v| v.checked_add(frac)) {
                if m < 1 << 53 {
                    let v = m as f64 / p as f64;
                    return Tok::Real(if neg { -v } else { v });
                }
            }
        }
        // Swallow a tail like "1.2.3" or "12e5" so no extra tokens are produced.
        while let Some(c) = self.peek() {
            if is_ws(c) || is_delim(c) {
                break;
            }
            self.pos += 1;
        }
        if digits == 0 {
            return Tok::Real(0.0);
        }
        if !real && !overflow {
            return Tok::Int(if neg { -int } else { int });
        }
        let txt: String = self.b[s..self.pos]
            .iter()
            .filter(|c| c.is_ascii_digit() || **c == b'.')
            .map(|&c| c as char)
            .collect();
        let mut v = parse_decimal(&txt);
        if neg {
            v = -v;
        }
        Tok::Real(v)
    }

    fn name(&mut self) -> Vec<u8> {
        let mut out = vec![];
        while let Some(c) = self.peek() {
            if is_ws(c) || is_delim(c) {
                break;
            }
            self.pos += 1;
            if c == b'#' {
                let h = self.b.get(self.pos).copied().and_then(hexval);
                let l = self.b.get(self.pos + 1).copied().and_then(hexval);
                if let (Some(h), Some(l)) = (h, l) {
                    out.push(h << 4 | l);
                    self.pos += 2;
                    continue;
                }
            }
            out.push(c);
        }
        out
    }

    fn hex_string(&mut self) -> Vec<u8> {
        let mut out = vec![];
        let mut hi: Option<u8> = None;
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == b'>' {
                break;
            }
            if let Some(v) = hexval(c) {
                match hi.take() {
                    Some(h) => out.push(h << 4 | v),
                    None => hi = Some(v),
                }
            }
        }
        if let Some(h) = hi {
            out.push(h << 4);
        }
        out
    }

    fn lit_string(&mut self) -> Vec<u8> {
        let mut out = vec![];
        let mut depth = 1usize;
        while let Some(c) = self.peek() {
            self.pos += 1;
            match c {
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(c);
                }
                b'\\' => {
                    let Some(e) = self.peek() else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        _ => out.push(e),
                    }
                }
                _ => out.push(c),
            }
        }
        out
    }
}

/// Decimal notation "123.456" without an exponent; more exact than accumulating digits in f64.
fn parse_decimal(s: &str) -> f64 {
    let s = if s.starts_with('.') {
        format!("0{s}")
    } else {
        s.to_string()
    };
    let s = s.trim_end_matches('.');
    // Extra dots (broken input) after the first are dropped.
    let mut parts = s.splitn(2, '.');
    let a = parts.next().unwrap_or("0");
    let b: String = parts.next().unwrap_or("").replace('.', "");
    let t = if b.is_empty() {
        a.to_string()
    } else {
        format!("{a}.{b}")
    };
    t.parse::<f64>().unwrap_or(0.0)
}

/// Object parsing; `refs` enables the "N G R" form (absent in content streams).
pub struct Parser<'a> {
    pub lx: Lexer<'a>,
    pub refs: bool,
}

impl<'a> Parser<'a> {
    pub fn new(b: &'a [u8], pos: usize, refs: bool) -> Self {
        Self {
            lx: Lexer::new(b, pos),
            refs,
        }
    }

    /// The next object, or `None` at the end/a keyword (the keyword is returned in `Err`).
    pub fn object(&mut self, depth: usize) -> Result<Obj, Tok<'a>> {
        let t = self.lx.next();
        self.obj_from(t, depth)
    }

    pub fn obj_from(&mut self, t: Tok<'a>, depth: usize) -> Result<Obj, Tok<'a>> {
        Ok(match t {
            Tok::Int(i) => {
                if self.refs && (0..=u32::MAX as i64).contains(&i) {
                    let save = self.lx.pos;
                    if let Tok::Int(g) = self.lx.next() {
                        if (0..=u16::MAX as i64).contains(&g) {
                            if let Tok::Keyword(b"R") = self.lx.next() {
                                return Ok(Obj::Ref(i as u32, g as u16));
                            }
                        }
                    }
                    self.lx.pos = save;
                }
                Obj::Int(i)
            }
            Tok::Real(r) => Obj::Real(r),
            Tok::Name(n) => Obj::Name(n),
            Tok::Str(s) => Obj::Str(s),
            Tok::ArrOpen => {
                if depth >= MAX_DEPTH {
                    return Err(Tok::Bad);
                }
                let mut v = vec![];
                loop {
                    match self.lx.next() {
                        Tok::ArrClose | Tok::Eof => break,
                        Tok::Bad => continue,
                        Tok::Keyword(b"endobj" | b"stream" | b"endstream" | b"obj") => break,
                        t => match self.obj_from(t, depth + 1) {
                            Ok(o) => v.push(o),
                            Err(Tok::Bad) => return Err(Tok::Bad),
                            Err(_) => {}
                        },
                    }
                }
                Obj::Array(v)
            }
            Tok::DictOpen => {
                if depth >= MAX_DEPTH {
                    return Err(Tok::Bad);
                }
                Obj::Dict(self.dict_body(depth)?)
            }
            Tok::Keyword(b"true") => Obj::Bool(true),
            Tok::Keyword(b"false") => Obj::Bool(false),
            Tok::Keyword(b"null") => Obj::Null,
            t => return Err(t),
        })
    }

    fn dict_body(&mut self, depth: usize) -> Result<Dict, Tok<'a>> {
        let mut d = Dict::default();
        loop {
            match self.lx.next() {
                Tok::DictClose | Tok::Eof => break,
                Tok::Name(k) => {
                    let save = self.lx.pos;
                    let t = self.lx.next();
                    match t {
                        Tok::DictClose => {
                            d.0.push((k, Obj::Null));
                            break;
                        }
                        t => match self.obj_from(t, depth + 1) {
                            Ok(v) => d.0.push((k, v)),
                            Err(Tok::Bad) => return Err(Tok::Bad),
                            Err(Tok::Keyword(b"endobj" | b"stream" | b"obj")) => {
                                self.lx.pos = save;
                                break;
                            }
                            Err(_) => {}
                        },
                    }
                }
                Tok::Keyword(b"endobj" | b"stream" | b"obj") => break,
                _ => {}
            }
        }
        Ok(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(s: &str) -> Obj {
        Parser::new(s.as_bytes(), 0, true).object(0).unwrap()
    }

    #[test]
    fn tokens() {
        assert_eq!(one("-.5"), Obj::Real(-0.5));
        assert_eq!(one("+17"), Obj::Int(17));
        assert_eq!(one("4."), Obj::Real(4.0));
        assert_eq!(one("/A#20B"), Obj::Name(b"A B".to_vec()));
        assert_eq!(one("(a\\(b\\)\\101\\\nc)"), Obj::Str(b"a(b)Ac".to_vec()));
        assert_eq!(one("<41 4>"), Obj::Str(b"A@".to_vec()));
        assert_eq!(
            one("[1 0 R 2]"),
            Obj::Array(vec![Obj::Ref(1, 0), Obj::Int(2)])
        );
        let d = one("<</A 1/B/C/D>>");
        let _ = &d;
        let d = d.dict().unwrap();
        assert_eq!(d.get(b"B"), Some(&Obj::Name(b"C".to_vec())));
        assert_eq!(d.get(b"D"), Some(&Obj::Null));
        assert_eq!(d.get(b"A"), Some(&Obj::Int(1)));
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let s = "[".repeat(10_000);
        assert!(Parser::new(s.as_bytes(), 0, true).object(0).is_err());
    }
}
