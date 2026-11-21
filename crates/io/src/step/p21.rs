// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! ISO 10303-21 exchange structure: header and data sections, simple and complex entity
//! instances and their parameters parsed into a [`Store`]; strings decoded from their control
//! directives. Errors name the entity, the byte offset and the line.

use std::collections::HashMap;

use crate::job::{Progress, CANCELLED};

#[derive(Clone, Debug, PartialEq)]
pub enum P {
    Int(i64),
    Real(f64),
    Str(String),
    Enum(String),
    Ref(u64),
    /// `$`
    Null,
    /// `*`
    Derived,
    List(Vec<P>),
    /// `LENGTH_MEASURE(1.)`
    Typed(String, Box<P>),
    Bin(String),
}

impl P {
    pub fn num(&self) -> Option<f64> {
        match self {
            P::Real(x) => Some(*x),
            P::Int(i) => Some(*i as f64),
            P::Typed(_, p) => p.num(),
            _ => None,
        }
    }

    pub fn id(&self) -> Option<u64> {
        match self {
            P::Ref(i) => Some(*i),
            P::Typed(_, p) => p.id(),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            P::Str(s) => Some(s),
            P::Typed(_, p) => p.text(),
            _ => None,
        }
    }

    pub fn en(&self) -> Option<&str> {
        match self {
            P::Enum(s) => Some(s),
            P::Typed(_, p) => p.en(),
            _ => None,
        }
    }

    pub fn list(&self) -> Option<&[P]> {
        match self {
            P::List(v) => Some(v),
            P::Typed(_, p) => p.list(),
            _ => None,
        }
    }

    /// `.T.` / `.F.`; `.U.` and anything else answer None.
    pub fn flag(&self) -> Option<bool> {
        match self.en()? {
            "T" => Some(true),
            "F" => Some(false),
            _ => None,
        }
    }

    pub fn ids(&self) -> Vec<u64> {
        self.list()
            .map(|l| l.iter().filter_map(P::id).collect())
            .unwrap_or_default()
    }

    pub fn nums(&self) -> Vec<f64> {
        self.list()
            .map(|l| l.iter().filter_map(P::num).collect())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
pub struct Rec {
    pub name: u32,
    pub args: Vec<P>,
}

#[derive(Clone, Debug)]
pub enum Ent {
    One(Rec),
    /// Complex instance `#n=(A(..)B(..))`: one record per partial entity.
    Many(Vec<Rec>),
}

impl Ent {
    pub fn recs(&self) -> &[Rec] {
        match self {
            Ent::One(r) => std::slice::from_ref(r),
            Ent::Many(v) => v,
        }
    }
}

#[derive(Default, Debug)]
pub struct Store {
    names: Vec<String>,
    index: HashMap<String, u32>,
    pub ents: HashMap<u64, Ent>,
    by_name: HashMap<u32, Vec<u64>>,
    pub schema: Vec<String>,
}

impl Store {
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.names.len() as u32;
        self.names.push(s.to_string());
        self.index.insert(s.to_string(), i);
        i
    }

    pub fn name(&self, r: &Rec) -> &str {
        &self.names[r.name as usize]
    }

    /// Entity type of a simple instance, the first partial type of a complex one.
    pub fn kind(&self, id: u64) -> Option<&str> {
        self.ents.get(&id).map(|e| self.name(&e.recs()[0]))
    }

    /// `NAME` or `(A B C)`, for messages.
    pub fn describe(&self, id: u64) -> String {
        match self.ents.get(&id) {
            None => format!("missing #{id}"),
            Some(Ent::One(r)) => self.name(r).to_string(),
            Some(Ent::Many(rs)) => {
                let n: Vec<&str> = rs.iter().map(|r| self.name(r)).collect();
                format!("({})", n.join(" "))
            }
        }
    }

    /// Arguments of the record `name`: the simple instance itself or one part of a complex one.
    pub fn rec(&self, id: u64, name: &str) -> Option<&[P]> {
        let n = *self.index.get(name)?;
        self.ents
            .get(&id)?
            .recs()
            .iter()
            .find(|r| r.name == n)
            .map(|r| &r.args[..])
    }

    pub fn is(&self, id: u64, name: &str) -> bool {
        self.rec(id, name).is_some()
    }

    /// Every instance with a record `name`, ascending.
    pub fn all(&self, name: &str) -> &[u64] {
        self.index
            .get(name)
            .and_then(|n| self.by_name.get(n))
            .map_or(&[], |v| &v[..])
    }

    pub fn len(&self) -> usize {
        self.ents.len()
    }
}

/// Parses a whole exchange file. Progress counts entity instances against an estimate.
pub fn parse(b: &[u8], progress: Option<&Progress>) -> Result<Store, String> {
    let total = b.iter().filter(|&&c| c == b'=').count().max(1) as u64;
    if let Some(p) = progress {
        p.set_total(total);
        p.set(0);
    }
    let mut ps = Parser {
        b,
        i: 0,
        cur: None,
        st: Store::default(),
        depth: 0,
        progress,
        count: 0,
        total,
    };
    ps.file()?;
    let mut st = ps.st;
    let mut by: HashMap<u32, Vec<u64>> = HashMap::new();
    for (&id, e) in &st.ents {
        for r in e.recs() {
            by.entry(r.name).or_default().push(id);
        }
    }
    for v in by.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    st.by_name = by;
    if let Some(p) = progress {
        p.set(total);
    }
    Ok(st)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    cur: Option<u64>,
    st: Store,
    depth: usize,
    progress: Option<&'a Progress>,
    count: u64,
    total: u64,
}

impl Parser<'_> {
    fn err(&self, msg: impl std::fmt::Display) -> String {
        let at = self.i.min(self.b.len());
        let line = self.b[..at].iter().filter(|&&c| c == b'\n').count() + 1;
        match self.cur {
            Some(id) => format!("entity #{id}, byte offset {at} (line {line}): {msg}"),
            None => format!("byte offset {at} (line {line}): {msg}"),
        }
    }

    fn eof(&self) -> String {
        self.err("unexpected end of file (truncated?)")
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn skip(&mut self) -> Result<(), String> {
        loop {
            while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                self.i += 1;
            }
            if !self.b[self.i..].starts_with(b"/*") {
                return Ok(());
            }
            match self.b[self.i + 2..].windows(2).position(|w| w == b"*/") {
                Some(k) => self.i += k + 4,
                None => return Err(self.err("unterminated comment (truncated?)")),
            }
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        self.skip()?;
        match self.peek() {
            Some(x) if x == c => {
                self.i += 1;
                Ok(())
            }
            Some(x) => Err(self.err(format!("expected '{}', found '{}'", c as char, x as char))),
            None => Err(self.eof()),
        }
    }

    fn keyword(&mut self) -> Result<String, String> {
        self.skip()?;
        let s = self.i;
        match self.peek() {
            None => return Err(self.eof()),
            Some(c) if c.is_ascii_alphabetic() || c == b'!' || c == b'_' => {}
            Some(c) => return Err(self.err(format!("expected a keyword, found '{}'", c as char))),
        }
        self.i += 1;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            self.i += 1;
        }
        Ok(String::from_utf8_lossy(&self.b[s..self.i]).to_ascii_uppercase())
    }

    fn digits(&mut self) -> Result<u64, String> {
        let s = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if s == self.i {
            return Err(self.err("expected an entity number"));
        }
        std::str::from_utf8(&self.b[s..self.i])
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| self.err("entity number out of range"))
    }

    fn file(&mut self) -> Result<(), String> {
        self.skip()?;
        if self.peek().is_none() {
            return Err(self.err("the file is empty"));
        }
        let start = self.keyword().ok();
        if start.as_deref() != Some("ISO-10303-21") {
            self.i = 0;
            return Err(self.err("not a STEP file: it does not start with ISO-10303-21"));
        }
        self.expect(b';')?;
        let mut data = false;
        loop {
            self.skip()?;
            if self.peek().is_none() {
                return Err(self.err("the file ends without END-ISO-10303-21 (truncated?)"));
            }
            let at = self.i;
            let k = self.keyword()?;
            match k.as_str() {
                "HEADER" => {
                    self.expect(b';')?;
                    self.header()?;
                }
                "DATA" => {
                    self.skip()?;
                    if self.peek() == Some(b'(') {
                        self.args()?;
                    }
                    self.expect(b';')?;
                    self.data()?;
                    data = true;
                }
                "END-ISO-10303-21" => {
                    self.skip()?;
                    if self.peek() == Some(b';') {
                        self.i += 1;
                    }
                    break;
                }
                _ => {
                    self.i = at;
                    return Err(self.err(format!("unexpected keyword {k}")));
                }
            }
        }
        if data {
            Ok(())
        } else {
            Err(self.err("the file has no DATA section"))
        }
    }

    fn header(&mut self) -> Result<(), String> {
        loop {
            self.skip()?;
            if self.peek().is_none() {
                return Err(self.err("unexpected end of file in the HEADER section (truncated?)"));
            }
            let k = self.keyword()?;
            if k == "ENDSEC" {
                return self.expect(b';');
            }
            let a = self.args()?;
            self.expect(b';')?;
            if k == "FILE_SCHEMA" {
                if let Some(l) = a.first().and_then(P::list) {
                    self.st.schema.extend(l.iter().filter_map(P::text).map(str::to_string));
                }
            }
        }
    }

    fn data(&mut self) -> Result<(), String> {
        loop {
            self.skip()?;
            match self.peek() {
                None => {
                    return Err(self.err("unexpected end of file in the DATA section (truncated?)"))
                }
                Some(b'#') => self.instance()?,
                Some(_) => {
                    let at = self.i;
                    let k = self.keyword()?;
                    if k == "ENDSEC" {
                        return self.expect(b';');
                    }
                    self.i = at;
                    return Err(self.err(format!(
                        "expected an entity instance #n=... or ENDSEC, found {k}"
                    )));
                }
            }
            self.count += 1;
            if self.count % 4096 == 0 {
                if let Some(p) = self.progress {
                    p.set(self.count.min(self.total));
                    if p.is_cancelled() {
                        return Err(CANCELLED.into());
                    }
                }
            }
        }
    }

    fn instance(&mut self) -> Result<(), String> {
        let start = self.i;
        self.i += 1;
        let id = self.digits()?;
        self.cur = Some(id);
        self.expect(b'=')?;
        self.skip()?;
        let ent = if self.peek() == Some(b'(') {
            self.i += 1;
            let mut recs = vec![];
            loop {
                self.skip()?;
                match self.peek() {
                    None => return Err(self.eof()),
                    Some(b')') => {
                        self.i += 1;
                        break;
                    }
                    Some(_) => {
                        let k = self.keyword()?;
                        let args = self.args()?;
                        let name = self.st.intern(&k);
                        recs.push(Rec { name, args });
                    }
                }
            }
            if recs.is_empty() {
                return Err(self.err("empty complex entity"));
            }
            Ent::Many(recs)
        } else {
            let k = self.keyword()?;
            let args = self.args()?;
            Ent::One(Rec {
                name: self.st.intern(&k),
                args,
            })
        };
        self.expect(b';')?;
        if self.st.ents.insert(id, ent).is_some() {
            self.i = start;
            return Err(self.err("the entity number is defined twice"));
        }
        self.cur = None;
        Ok(())
    }

    fn args(&mut self) -> Result<Vec<P>, String> {
        self.expect(b'(')?;
        self.depth += 1;
        if self.depth > 200 {
            return Err(self.err("lists nested too deep"));
        }
        let mut v = vec![];
        self.skip()?;
        if self.peek() == Some(b')') {
            self.i += 1;
            self.depth -= 1;
            return Ok(v);
        }
        loop {
            v.push(self.param()?);
            self.skip()?;
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b')') => {
                    self.i += 1;
                    break;
                }
                Some(c) => {
                    return Err(self.err(format!("expected ',' or ')', found '{}'", c as char)))
                }
                None => return Err(self.eof()),
            }
        }
        self.depth -= 1;
        Ok(v)
    }

    fn param(&mut self) -> Result<P, String> {
        self.skip()?;
        let c = self.peek().ok_or_else(|| self.eof())?;
        match c {
            b'$' => {
                self.i += 1;
                Ok(P::Null)
            }
            b'*' => {
                self.i += 1;
                Ok(P::Derived)
            }
            b'#' | b'@' => {
                self.i += 1;
                Ok(P::Ref(self.digits()?))
            }
            b'\'' => self.string(),
            b'"' => {
                self.i += 1;
                let s = self.i;
                while self.peek().is_some_and(|c| c != b'"') {
                    self.i += 1;
                }
                if self.peek().is_none() {
                    return Err(self.eof());
                }
                let t = String::from_utf8_lossy(&self.b[s..self.i]).into_owned();
                self.i += 1;
                Ok(P::Bin(t))
            }
            b'.' => {
                self.i += 1;
                let s = self.i;
                loop {
                    match self.peek() {
                        None => return Err(self.eof()),
                        Some(b'.') => break,
                        Some(c) if c.is_ascii_alphanumeric() || c == b'_' => self.i += 1,
                        Some(c) => {
                            return Err(self.err(format!(
                                "bad character '{}' in an enumeration",
                                c as char
                            )))
                        }
                    }
                }
                let e = String::from_utf8_lossy(&self.b[s..self.i]).to_ascii_uppercase();
                self.i += 1;
                Ok(P::Enum(e))
            }
            b'(' => Ok(P::List(self.args()?)),
            b'+' | b'-' | b'0'..=b'9' => self.number(),
            c if c.is_ascii_alphabetic() || c == b'!' => {
                let k = self.keyword()?;
                let mut a = self.args()?;
                let inner = if a.len() == 1 { a.pop().unwrap_or(P::Null) } else { P::List(a) };
                Ok(P::Typed(k, Box::new(inner)))
            }
            _ => Err(self.err(format!("unexpected character '{}'", c as char))),
        }
    }

    fn number(&mut self) -> Result<P, String> {
        let s = self.i;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.' | b'e' | b'E'))
        {
            self.i += 1;
        }
        let t = std::str::from_utf8(&self.b[s..self.i]).unwrap_or("");
        let real = t.contains(|c| matches!(c, '.' | 'e' | 'E'));
        if !real {
            if let Ok(i) = t.parse::<i64>() {
                return Ok(P::Int(i));
            }
        }
        // num-lint: STEP real literal, invariant form by the file format
        match t.parse::<f64>() {
            Ok(x) if x.is_finite() => Ok(P::Real(x)),
            _ => {
                self.i = s;
                Err(self.err(format!("bad number '{t}'")))
            }
        }
    }

    fn string(&mut self) -> Result<P, String> {
        self.i += 1;
        let mut raw = Vec::new();
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated string (truncated?)")),
                Some(b'\'') => {
                    if self.b.get(self.i + 1) == Some(&b'\'') {
                        raw.extend_from_slice(b"''");
                        self.i += 2;
                    } else {
                        self.i += 1;
                        break;
                    }
                }
                // writers wrap long strings; line breaks are not part of the value
                Some(b'\r') | Some(b'\n') => self.i += 1,
                Some(c) => {
                    raw.push(c);
                    self.i += 1;
                }
            }
        }
        Ok(P::Str(decode(&raw)))
    }
}

fn hex(b: &[u8]) -> Option<u32> {
    std::str::from_utf8(b).ok().and_then(|t| u32::from_str_radix(t, 16).ok())
}

fn push_char(out: &mut Vec<u8>, c: char) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

/// Control directives of a string value: `\\`, `\X\hh` (ISO 8859-1), `\X2\..\X0\` (UTF-16),
/// `\X4\..\X0\` (UTF-32), `\S\c`, `\P?\`. Other bytes are taken as UTF-8.
pub fn decode(raw: &[u8]) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let rest = &raw[i..];
        if rest.starts_with(b"''") {
            out.push(b'\'');
            i += 2;
            continue;
        }
        if raw[i] == b'\\' {
            if rest.starts_with(b"\\\\") {
                out.push(b'\\');
                i += 2;
                continue;
            }
            if rest.starts_with(b"\\X2\\") || rest.starts_with(b"\\X4\\") {
                let w = if rest[2] == b'2' { 4 } else { 8 };
                let mut j = i + 4;
                let mut units: Vec<u32> = vec![];
                while j + w <= raw.len() && raw[j] != b'\\' {
                    match hex(&raw[j..j + w]) {
                        Some(u) => units.push(u),
                        None => break,
                    }
                    j += w;
                }
                if raw[j..].starts_with(b"\\X0\\") {
                    j += 4;
                }
                if w == 4 {
                    let u16s: Vec<u16> = units.iter().map(|&u| u as u16).collect();
                    for c in char::decode_utf16(u16s) {
                        push_char(&mut out, c.unwrap_or(char::REPLACEMENT_CHARACTER));
                    }
                } else {
                    for u in units {
                        push_char(&mut out, char::from_u32(u).unwrap_or(char::REPLACEMENT_CHARACTER));
                    }
                }
                i = j;
                continue;
            }
            if rest.len() >= 5 && rest.starts_with(b"\\X\\") {
                if let Some(h) = hex(&rest[3..5]) {
                    push_char(&mut out, char::from_u32(h).unwrap_or(char::REPLACEMENT_CHARACTER));
                    i += 5;
                    continue;
                }
            }
            if rest.len() >= 4 && rest.starts_with(b"\\S\\") {
                push_char(&mut out, char::from_u32(u32::from(rest[3]) + 128).unwrap_or('?'));
                i += 4;
                continue;
            }
            if rest.len() >= 4 && rest[1] == b'P' && rest[3] == b'\\' {
                i += 4;
                continue;
            }
            if rest.starts_with(b"\\N\\") || rest.starts_with(b"\\F\\") {
                i += 3;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    match String::from_utf8(out) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| char::from(b)).collect(),
    }
}
