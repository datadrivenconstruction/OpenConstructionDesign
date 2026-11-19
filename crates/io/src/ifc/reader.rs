// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! STEP physical file reader (ISO 10303-21): one pass over the bytes, typed values,
//! string escapes decoded, every error names its line and entity; references are checked.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::hash::{BuildHasherDefault, Hasher};
use std::ops::Deref;
use std::sync::Arc;

mod pages;
mod stream;
pub use stream::{OpenError, StreamOptions};

/// Multiplicative hash for entity ids (ids are already well spread; SipHash dominated parsing).
#[derive(Default)]
struct IdHash(u64);

impl Hasher for IdHash {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, b: &[u8]) {
        for x in b {
            self.0 = (self.0.rotate_left(5) ^ *x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }
    fn write_u32(&mut self, v: u32) {
        self.0 = (v as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

type IdMap<V> = HashMap<u32, V, BuildHasherDefault<IdHash>>;

/// Nesting limit for lists/typed values: hostile input cannot overflow the stack.
const MAX_DEPTH: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `$` — unset.
    Null,
    /// `*` — derived.
    Derived,
    Int(i64),
    Real(f64),
    Str(String),
    /// `.NAME.` without dots.
    Enum(String),
    Ref(u32),
    List(Vec<Value>),
    /// `IFCLABEL('x')`-style typed parameter.
    Typed(String, Vec<Value>),
    /// `"0FF"` binary, kept as hex text.
    Binary(String),
}

impl Value {
    pub fn as_ref(&self) -> Option<u32> {
        match self {
            Value::Ref(r) => Some(*r),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Real(v) => Some(*v),
            Value::Int(v) => Some(*v as f64),
            Value::Typed(_, a) if a.len() == 1 => a[0].as_f64(),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Typed(_, a) if a.len() == 1 => a[0].as_str(),
            _ => None,
        }
    }
    pub fn as_enum(&self) -> Option<&str> {
        match self {
            Value::Enum(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(v) => Some(v),
            _ => None,
        }
    }
    /// The reference, or the references of a list (anything else is skipped).
    pub fn refs(&self) -> Vec<u32> {
        match self {
            Value::Ref(r) => vec![*r],
            Value::List(l) => l.iter().filter_map(Value::as_ref).collect(),
            _ => vec![],
        }
    }
}

/// [`Value::refs`] of an optional attribute.
pub fn refs_of(v: Option<&Value>) -> Vec<u32> {
    v.map(Value::refs).unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepErrorKind {
    NotStep,
    Eof,
    Unexpected(char),
    Expected(&'static str),
    BadNumber(String),
    BadString(&'static str),
    BadId,
    DuplicateId,
    MissingRef(u32),
    TooDeep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepError {
    pub line: usize,
    pub entity: Option<u32>,
    pub kind: StepErrorKind,
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "STEP line {}", self.line)?;
        if let Some(e) = self.entity {
            write!(f, ", #{e}")?;
        }
        match &self.kind {
            StepErrorKind::NotStep => write!(f, ": not an ISO-10303-21 file"),
            StepErrorKind::Eof => write!(f, ": unexpected end of file"),
            StepErrorKind::Unexpected(c) => write!(f, ": unexpected {c:?}"),
            StepErrorKind::Expected(w) => write!(f, ": expected {w}"),
            StepErrorKind::BadNumber(n) => write!(f, ": bad number {n:?}"),
            StepErrorKind::BadString(w) => write!(f, ": bad string ({w})"),
            StepErrorKind::BadId => write!(f, ": bad entity id"),
            StepErrorKind::DuplicateId => write!(f, ": duplicate entity id"),
            StepErrorKind::MissingRef(r) => write!(f, ": reference to missing #{r}"),
            StepErrorKind::TooDeep => write!(f, ": nesting deeper than {MAX_DEPTH}"),
        }
    }
}

impl std::error::Error for StepError {}

/// One DATA entity. Arguments are typed on first access: the validating pass does not
/// allocate per value, so a 100 MB file parses within the 2 s budget.
pub struct Entity {
    ty: u32,
    pub line: u32,
    span: (usize, usize),
    args: OnceCell<Vec<Value>>,
}

impl Entity {
    /// Typed arguments; filled by [`StepFile::get`] before the entity is handed out.
    pub fn args(&self) -> &[Value] {
        self.args.get().map_or(&[], |v| v.as_slice())
    }
}

/// Arguments of an entity from [`StepFile::leaf`]: borrowed from the file, or shared from
/// the bounded cache of a streamed file.
pub enum Args<'a> {
    Borrowed(&'a [Value]),
    Shared(Arc<[Value]>),
}

impl Deref for Args<'_> {
    type Target = [Value];
    fn deref(&self) -> &[Value] {
        match self {
            Args::Borrowed(a) => a,
            Args::Shared(a) => a,
        }
    }
}

/// Parsed file: header schema names and the DATA entities by id (file order kept).
#[derive(Default)]
pub struct StepFile {
    pub schema: Vec<String>,
    src: Vec<u8>,
    types: Vec<String>,
    ents: IdMap<Entity>,
    order: Vec<u32>,
    /// Streamed file ([`StepFile::open`]): compact slots instead of `ents`, `src` stays empty.
    slots: IdMap<stream::Slot>,
    lazy: Option<stream::Lazy>,
    /// Tolerant parse or open only: duplicate ids (first kept) and dangling references (left unresolved).
    pub warnings: Vec<String>,
}

impl StepFile {
    pub fn parse(bytes: &[u8]) -> Result<Self, StepError> {
        Self::parse_with(bytes, true)
    }

    /// Like [`Self::parse`], but a duplicate id or a reference to a missing entity becomes a
    /// warning instead of an error; syntax errors still fail.
    pub fn parse_tolerant(bytes: &[u8]) -> Result<Self, StepError> {
        Self::parse_with(bytes, false)
    }

    fn parse_with(bytes: &[u8], strict: bool) -> Result<Self, StepError> {
        let mut p = P::new(bytes, false);
        p.lenient = !strict;
        let mut f = StepFile::default();
        f.ents.reserve(bytes.len() / 64);
        f.order.reserve(bytes.len() / 64);
        let mut type_ix: HashMap<Vec<u8>, u32> = HashMap::new();
        let mut refs: Vec<(u32, u32)> = vec![];
        p.ws()?;
        p.word("ISO-10303-21")?;
        p.ws()?;
        p.eat(b';')?;
        p.ws()?;
        p.word("HEADER")?;
        p.ws()?;
        p.eat(b';')?;
        loop {
            p.ws()?;
            let k = p.keyword()?;
            if k == b"ENDSEC" {
                p.ws()?;
                p.eat(b';')?;
                break;
            }
            p.ws()?;
            p.build = true;
            let (at, line) = (p.i, p.line);
            let parsed = p.params(0).and_then(|a| {
                p.ws()?;
                p.eat(b';')?;
                Ok(a)
            });
            p.build = false;
            let args = match parsed {
                Ok(a) => a,
                // a malformed header record carries no geometry: skip it, keep the file
                Err(e) if !strict && k != b"FILE_SCHEMA" => {
                    (p.i, p.line) = (at, line);
                    p.skip_record()?;
                    f.warn(format!(
                        "header {}: {e}; skipped",
                        String::from_utf8_lossy(&k)
                    ));
                    continue;
                }
                Err(e) => return Err(e),
            };
            if k == b"FILE_SCHEMA" {
                if let Some(Value::List(l)) = args.first() {
                    f.schema = l
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect();
                }
            }
        }
        loop {
            p.ws()?;
            let k = p.keyword()?;
            if k == b"END-ISO-10303-21" {
                p.ws()?;
                p.eat(b';')?;
                break;
            }
            if k != b"DATA" {
                return Err(p.err(StepErrorKind::Expected("DATA or END-ISO-10303-21")));
            }
            p.ws()?;
            if p.peek() == Some(b'(') {
                p.params(0)?;
                p.ws()?;
            }
            p.eat(b';')?;
            loop {
                p.ws()?;
                match p.peek() {
                    Some(b'#') => {}
                    Some(_) => {
                        p.word("ENDSEC")?;
                        p.ws()?;
                        p.eat(b';')?;
                        break;
                    }
                    None => return Err(p.err(StepErrorKind::Eof)),
                }
                p.i += 1;
                let line = p.line;
                let id = p.uint().ok_or_else(|| p.err(StepErrorKind::BadId))?;
                p.ent = Some(id);
                p.ws()?;
                p.eat(b'=')?;
                p.ws()?;
                let start = p.i;
                p.refs.clear();
                let (name, _) = p.body()?;
                let end = p.i;
                p.ws()?;
                p.eat(b';')?;
                let ty = match type_ix.get(name) {
                    Some(t) => *t,
                    None => {
                        let t = f.types.len() as u32;
                        f.types.push(latin(name));
                        type_ix.insert(name.to_vec(), t);
                        t
                    }
                };
                refs.extend(p.refs.iter().map(|r| (id, *r)));
                let e = Entity {
                    ty,
                    line: line as u32,
                    span: (start, end),
                    args: OnceCell::new(),
                };
                if f.ents.contains_key(&id) {
                    if strict {
                        return Err(StepError {
                            line,
                            entity: Some(id),
                            kind: StepErrorKind::DuplicateId,
                        });
                    }
                    f.warn(format!("line {line}: duplicate #{id} ignored"));
                    // keep the first definition; references from the duplicate are not checked
                    refs.truncate(refs.len() - p.refs.len());
                    p.ent = None;
                    continue;
                }
                f.ents.insert(id, e);
                f.order.push(id);
                p.ent = None;
            }
        }
        let mut dangling = 0usize;
        for (from, to) in refs {
            if !f.ents.contains_key(&to) {
                let line = f.ents.get(&from).map_or(0, |e| e.line as usize);
                if strict {
                    return Err(StepError {
                        line,
                        entity: Some(from),
                        kind: StepErrorKind::MissingRef(to),
                    });
                }
                dangling += 1;
                if dangling <= 20 {
                    f.warn(format!("line {line}: #{from} refers to missing #{to}"));
                }
            }
        }
        if dangling > 20 {
            f.warn(format!("{} more dangling references", dangling - 20));
        }
        if p.lone > 0 {
            f.warn(lone_warning(p.lone, p.lone_line));
        }
        f.src = bytes.to_vec();
        Ok(f)
    }

    fn warn(&mut self, w: String) {
        if self.warnings.len() < 200 {
            self.warnings.push(w);
        }
    }

    /// Entity with its arguments; a streamed file keeps what it hands out decoded (pinned)
    /// for its life. None for a missing id, and for a streamed file once cancelled.
    pub fn get(&self, id: u32) -> Option<&Entity> {
        if let Some(lz) = &self.lazy {
            return lz.entity(id, self.slots.get(&id)?);
        }
        let e = self.ents.get(&id)?;
        e.args.get_or_init(|| {
            let mut p = P::new(&self.src[..e.span.1], true);
            p.lenient = true;
            p.i = e.span.0;
            // the validating pass accepted this span, so a failure here cannot happen
            p.body().map(|b| b.1).unwrap_or_default()
        });
        Some(e)
    }

    /// Arguments of an entity read once and dropped (points, loops, faces, properties,
    /// relations): a streamed file serves them from a bounded cache and does not pin them.
    pub fn leaf(&self, id: u32) -> Option<Args<'_>> {
        match &self.lazy {
            Some(lz) => lz.leaf(id, self.slots.get(&id)?),
            None => self.get(id).map(|e| Args::Borrowed(e.args())),
        }
    }

    pub fn type_of(&self, id: u32) -> Option<&str> {
        let t = match &self.lazy {
            Some(_) => self.slots.get(&id)?.ty(),
            None => self.ents.get(&id)?.ty as usize,
        };
        Some(self.types[t].as_str())
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Attributes of entity `id`; empty when there is none.
    pub fn args(&self, id: u32) -> &[Value] {
        self.get(id).map_or(&[], |e| e.args())
    }

    /// String attribute `i` of entity `id`.
    pub fn str_at(&self, id: u32, i: usize) -> Option<&str> {
        self.args(id).get(i)?.as_str()
    }

    /// Entity ids per upper-case type name, in file order.
    pub fn by_class(&self) -> BTreeMap<&str, Vec<u32>> {
        let mut by: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
        for (id, t) in self.iter() {
            by.entry(t).or_default().push(id);
        }
        by
    }

    /// Entity ids with upper-case type names, in file order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &str)> {
        self.order
            .iter()
            .map(move |id| (*id, self.type_of(*id).unwrap_or_default()))
    }
}

/// Single per-file warning for string escapes kept literally (tolerant parse only).
fn lone_warning(n: u32, line: usize) -> String {
    format!("line {line}: {n} unknown backslash escape(s) in strings kept literally")
}

fn latin(b: &[u8]) -> String {
    b.iter().map(|c| *c as char).collect()
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
    line: usize,
    ent: Option<u32>,
    /// false: validate only (no per-value allocation), refs collected into `refs`.
    build: bool,
    refs: Vec<u32>,
    /// Shared value stack: each list is collected once at its exact size.
    stack: Vec<Value>,
    sbuf: String,
    rbuf: Vec<u8>,
    ubuf: Vec<u16>,
    /// Unknown string escapes are kept literally instead of failing.
    lenient: bool,
    /// Escapes kept literally so far, and the line of the first.
    lone: u32,
    lone_line: usize,
}

impl<'a> P<'a> {
    fn new(b: &'a [u8], build: bool) -> Self {
        P {
            lenient: false,
            lone: 0,
            lone_line: 0,
            b,
            i: 0,
            line: 1,
            ent: None,
            build,
            refs: vec![],
            stack: Vec::with_capacity(256),
            sbuf: String::new(),
            rbuf: vec![],
            ubuf: vec![],
        }
    }

    fn err(&self, kind: StepErrorKind) -> StepError {
        StepError {
            line: self.line,
            entity: self.ent,
            kind,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn unexpected(&self) -> StepError {
        match self.peek() {
            Some(c) => self.err(StepErrorKind::Unexpected(c as char)),
            None => self.err(StepErrorKind::Eof),
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), StepError> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    /// Past the next `;` outside quoted strings.
    fn skip_record(&mut self) -> Result<(), StepError> {
        let mut quoted = false;
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                b'\n' => self.line += 1,
                b'\'' => quoted = !quoted,
                b';' if !quoted => return Ok(()),
                _ => {}
            }
        }
        Err(self.err(StepErrorKind::Eof))
    }

    /// Whitespace and `/* */` comments.
    fn ws(&mut self) -> Result<(), StepError> {
        while let Some(c) = self.peek() {
            match c {
                b'\n' => {
                    self.line += 1;
                    self.i += 1;
                }
                b' ' | b'\t' | b'\r' => self.i += 1,
                b'/' if self.b.get(self.i + 1) == Some(&b'*') => {
                    self.i += 2;
                    loop {
                        match self.peek() {
                            None => return Err(self.err(StepErrorKind::Eof)),
                            Some(b'*') if self.b.get(self.i + 1) == Some(&b'/') => {
                                self.i += 2;
                                break;
                            }
                            Some(b'\n') => {
                                self.line += 1;
                                self.i += 1;
                            }
                            Some(_) => self.i += 1,
                        }
                    }
                }
                _ => break,
            }
        }
        Ok(())
    }

    fn keyword(&mut self) -> Result<&'a [u8], StepError> {
        let b = self.b;
        let s = self.i;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
                self.i += 1;
            } else {
                break;
            }
        }
        if s == self.i || !b[s].is_ascii_alphabetic() {
            return Err(self.unexpected());
        }
        Ok(&b[s..self.i])
    }

    fn word(&mut self, w: &'static str) -> Result<(), StepError> {
        let s = self.i;
        let k = self
            .keyword()
            .map_err(|_| self.err(StepErrorKind::Expected(w)))?;
        if !k.eq_ignore_ascii_case(w.as_bytes()) {
            self.i = s;
            let kind = if w == "ISO-10303-21" {
                StepErrorKind::NotStep
            } else {
                StepErrorKind::Expected(w)
            };
            return Err(self.err(kind));
        }
        Ok(())
    }

    fn uint(&mut self) -> Option<u32> {
        let s = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        std::str::from_utf8(&self.b[s..self.i]).ok()?.parse().ok()
    }

    /// Entity body: `NAME(args)` or complex `(A(..) B(..))`.
    fn body(&mut self) -> Result<(&'a [u8], Vec<Value>), StepError> {
        if self.peek() == Some(b'(') {
            self.i += 1;
            let mut parts = vec![];
            loop {
                self.ws()?;
                if self.peek() == Some(b')') {
                    self.i += 1;
                    break;
                }
                let n = self.keyword()?;
                self.ws()?;
                let a = self.params(1)?;
                if self.build {
                    parts.push(Value::Typed(latin(n), a));
                }
            }
            Ok((b"COMPLEX".as_slice(), parts))
        } else {
            let n = self.keyword()?;
            self.ws()?;
            Ok((n, self.params(0)?))
        }
    }

    /// `( v, v, ... )`; empty Vec when not building.
    fn params(&mut self, depth: usize) -> Result<Vec<Value>, StepError> {
        if depth > MAX_DEPTH {
            return Err(self.err(StepErrorKind::TooDeep));
        }
        self.eat(b'(')?;
        self.ws()?;
        if self.peek() == Some(b')') {
            self.i += 1;
            return Ok(vec![]);
        }
        let start = self.stack.len();
        loop {
            self.ws()?;
            let v = self.value(depth)?;
            if self.build {
                self.stack.push(v);
            }
            self.ws()?;
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b')') => {
                    self.i += 1;
                    return Ok(self.stack.drain(start..).collect());
                }
                _ => return Err(self.unexpected()),
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, StepError> {
        let c = self.peek().ok_or_else(|| self.err(StepErrorKind::Eof))?;
        Ok(match c {
            b'$' => {
                self.i += 1;
                Value::Null
            }
            b'*' => {
                self.i += 1;
                Value::Derived
            }
            b'#' => {
                self.i += 1;
                let r = self.uint().ok_or_else(|| self.err(StepErrorKind::BadId))?;
                if !self.build {
                    self.refs.push(r);
                }
                Value::Ref(r)
            }
            b'\'' => {
                self.string()?;
                if self.build {
                    Value::Str(self.sbuf.clone())
                } else {
                    Value::Null
                }
            }
            b'"' => {
                self.i += 1;
                let s = self.i;
                while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                    self.i += 1;
                }
                let h = &self.b[s..self.i];
                self.eat(b'"')?;
                if self.build {
                    Value::Binary(latin(h))
                } else {
                    Value::Null
                }
            }
            b'.' if self
                .b
                .get(self.i + 1)
                .is_some_and(|c| c.is_ascii_alphabetic()) =>
            {
                self.i += 1;
                let k = self.keyword()?;
                self.eat(b'.')?;
                if self.build {
                    Value::Enum(latin(k))
                } else {
                    Value::Null
                }
            }
            b'(' => Value::List(self.params(depth + 1)?),
            b'0'..=b'9' | b'+' | b'-' | b'.' => self.number()?,
            c if c.is_ascii_alphabetic() => {
                let k = self.keyword()?;
                self.ws()?;
                let a = self.params(depth + 1)?;
                if self.build {
                    Value::Typed(latin(k), a)
                } else {
                    Value::Null
                }
            }
            _ => return Err(self.unexpected()),
        })
    }

    fn number(&mut self) -> Result<Value, StepError> {
        let s = self.i;
        let mut real = false;
        while let Some(c) = self.peek() {
            match c {
                b'0'..=b'9' | b'+' | b'-' => {}
                b'.' | b'E' | b'e' => real = true,
                _ => break,
            }
            self.i += 1;
        }
        let b = self.b;
        let t = std::str::from_utf8(&b[s..self.i]).unwrap_or("");
        let bad = || self.err(StepErrorKind::BadNumber(t.to_string()));
        if real {
            let v: f64 = t.parse().map_err(|_| bad())?;
            if !v.is_finite() {
                return Err(bad());
            }
            Ok(Value::Real(v))
        } else {
            // an integer past i64 is still a valid number: keep it as a real
            match t.parse() {
                Ok(v) => Ok(Value::Int(v)),
                Err(_) => match t.parse::<f64>() {
                    Ok(v) if v.is_finite() && t.bytes().any(|c| c.is_ascii_digit()) => {
                        Ok(Value::Real(v))
                    }
                    _ => Err(bad()),
                },
            }
        }
    }

    fn hex(&self, from: usize, n: usize) -> Option<u32> {
        let h = self.b.get(from..from + n)?;
        if !h.iter().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()
    }

    /// `'...'` into `sbuf`, with `''`, `\\`, `\X2\..\X0\`, `\X4\..\X0\`, `\X\hh`, `\S\c`, `\Px\`.
    fn string(&mut self) -> Result<(), StepError> {
        self.i += 1;
        self.sbuf.clear();
        let b = self.b;
        let rest = &b[self.i..];
        if let Some(n) = rest.iter().position(|c| matches!(c, b'\'' | b'\\' | b'\n')) {
            if rest[n] == b'\'' && rest.get(n + 1) != Some(&b'\'') {
                self.i += n + 1;
                self.sbuf.push_str(&String::from_utf8_lossy(&rest[..n]));
                return Ok(());
            }
        }
        let mut out = std::mem::take(&mut self.sbuf);
        let mut raw = std::mem::take(&mut self.rbuf);
        let mut units = std::mem::take(&mut self.ubuf);
        raw.clear();
        let r = self.string_slow(&mut out, &mut raw, &mut units);
        self.sbuf = out;
        self.rbuf = raw;
        self.ubuf = units;
        r
    }

    fn string_slow(
        &mut self,
        out: &mut String,
        raw: &mut Vec<u8>,
        units: &mut Vec<u16>,
    ) -> Result<(), StepError> {
        let flush = |raw: &mut Vec<u8>, out: &mut String| {
            out.push_str(&String::from_utf8_lossy(raw));
            raw.clear();
        };
        let bad = |p: &Self, w| p.err(StepErrorKind::BadString(w));
        // code page of `\S\c`: 1 = ISO 8859-1 .. 9 = 8859-9, reset for every string
        let mut page = 1u8;
        loop {
            let c = self.peek().ok_or_else(|| self.err(StepErrorKind::Eof))?;
            match c {
                b'\'' if self.b.get(self.i + 1) == Some(&b'\'') => {
                    raw.push(b'\'');
                    self.i += 2;
                }
                b'\'' => {
                    self.i += 1;
                    flush(raw, out);
                    return Ok(());
                }
                b'\\' => {
                    let rest = &self.b[self.i..];
                    if rest.starts_with(b"\\\\") {
                        raw.push(b'\\');
                        self.i += 2;
                    } else if rest.starts_with(b"\\X2\\") || rest.starts_with(b"\\X4\\") {
                        let n = if rest[2] == b'2' { 4 } else { 8 };
                        self.i += 4;
                        flush(raw, out);
                        units.clear();
                        loop {
                            if self.b[self.i..].starts_with(b"\\X0\\") {
                                self.i += 4;
                                break;
                            }
                            let v = self.hex(self.i, n).ok_or_else(|| bad(self, "hex"))?;
                            self.i += n;
                            if n == 4 {
                                units.push(v as u16);
                            } else {
                                out.push(char::from_u32(v).ok_or_else(|| bad(self, "code"))?);
                            }
                        }
                        for ch in char::decode_utf16(units.iter().copied()) {
                            out.push(ch.map_err(|_| bad(self, "utf-16"))?);
                        }
                    } else if rest.starts_with(b"\\X\\") {
                        let v = self.hex(self.i + 3, 2).ok_or_else(|| bad(self, "hex"))?;
                        flush(raw, out);
                        out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
                        self.i += 5;
                    } else if rest.starts_with(b"\\S\\") && rest.len() > 3 {
                        flush(raw, out);
                        out.push(pages::map(page, rest[3]));
                        self.i += 4;
                    } else if rest.len() >= 4 && rest[1] == b'P' && rest[3] == b'\\' {
                        if (b'A'..=b'I').contains(&rest[2]) {
                            page = rest[2] - b'A' + 1;
                        }
                        self.i += 4;
                    } else if self.lenient {
                        // a lone backslash (a path such as C:\Temp) stays as written
                        if self.lone == 0 {
                            self.lone_line = self.line;
                        }
                        self.lone += 1;
                        raw.push(b'\\');
                        self.i += 1;
                    } else {
                        return Err(bad(self, "escape"));
                    }
                }
                b'\n' => {
                    self.line += 1;
                    raw.push(c);
                    self.i += 1;
                }
                _ => {
                    raw.push(c);
                    self.i += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ifc::step::text;

    fn wrap(data: &str) -> String {
        format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{data}\nENDSEC;\nEND-ISO-10303-21;\n"
        )
    }

    #[test]
    fn typed_values() {
        let s = wrap(
            "#1=A($,*,12,-1.5E-3,2.,'it''s',.T.,#2,(1,(2,#2)),IFCLABEL('x'),\"0FF\");\n#2=B();",
        );
        let f = StepFile::parse(s.as_bytes()).unwrap();
        assert_eq!(f.schema, vec!["IFC4"]);
        let a = &f.get(1).unwrap().args();
        assert_eq!(a[0], Value::Null);
        assert_eq!(a[1], Value::Derived);
        assert_eq!(a[2], Value::Int(12));
        assert_eq!(a[3], Value::Real(-1.5e-3));
        assert_eq!(a[4], Value::Real(2.0));
        assert_eq!(a[5], Value::Str("it's".into()));
        assert_eq!(a[6], Value::Enum("T".into()));
        assert_eq!(a[7], Value::Ref(2));
        assert_eq!(
            a[8],
            Value::List(vec![
                Value::Int(1),
                Value::List(vec![Value::Int(2), Value::Ref(2)])
            ])
        );
        assert_eq!(a[9].as_str(), Some("x"));
        assert_eq!(a[10], Value::Binary("0FF".into()));
        assert_eq!(f.type_of(2), Some("B"));
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn unicode_escapes() {
        let s = wrap(
            r"#1=A('\X2\042D044204300436\X0\ 1','\X4\0001F600\X0\','\X\E9\S\a','a\\b','\PA\x');",
        );
        let f = StepFile::parse(s.as_bytes()).unwrap();
        let a = &f.get(1).unwrap().args();
        assert_eq!(a[0].as_str(), Some(crate::ru_fixtures::ru("storey_1")));
        assert_eq!(a[1].as_str(), Some("😀"));
        assert_eq!(a[2].as_str(), Some("éá"));
        assert_eq!(a[3].as_str(), Some("a\\b"));
        assert_eq!(a[4].as_str(), Some("x"));
    }

    #[test]
    fn writer_text_round_trips() {
        let mut seed = 7u64;
        for _ in 0..300 {
            let mut st = String::new();
            for _ in 0..(seed % 13) {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let pick = [
                    'a', '\'', '\\', '\u{416}', 'é', '😀', ' ', '中', '\n', 'z', '$', '#',
                ];
                st.push(pick[(seed >> 33) as usize % pick.len()]);
            }
            seed = seed.wrapping_add(1);
            let src = wrap(&format!("#1=A({});", text(&st)));
            let f = StepFile::parse(src.as_bytes()).unwrap();
            assert_eq!(f.get(1).unwrap().args()[0].as_str(), Some(st.as_str()));
        }
    }

    fn err(data: &str) -> StepError {
        StepFile::parse(wrap(data).as_bytes())
            .err()
            .expect("must fail")
    }

    #[test]
    fn errors_name_line_and_entity() {
        let e = err("#1=A(1);\n#2=B(1.2.3);");
        assert_eq!((e.line, e.entity), (8, Some(2)));
        assert!(matches!(e.kind, StepErrorKind::BadNumber(_)));
        assert_eq!(err("#1=A();\n#1=A();").kind, StepErrorKind::DuplicateId);
        let e = err("#1=A(#9);");
        assert_eq!((e.entity, e.kind), (Some(1), StepErrorKind::MissingRef(9)));
        assert!(matches!(err("#1=A('x);").kind, StepErrorKind::Eof));
        assert_eq!(
            err(r"#1=A('\X2\04\X0\');").kind,
            StepErrorKind::BadString("hex")
        );
        assert_eq!(
            err(r"#1=A('\X2\D800\X0\');").kind,
            StepErrorKind::BadString("utf-16")
        );
        assert_eq!(err(r"#1=A('\Q');").kind, StepErrorKind::BadString("escape"));
        assert!(matches!(
            err("#1=A(1E999);").kind,
            StepErrorKind::BadNumber(_)
        ));
        let deep = format!("#1=A({}{});", "(".repeat(5000), ")".repeat(5000));
        assert_eq!(err(&deep).kind, StepErrorKind::TooDeep);
        let e = StepFile::parse(b"hello").err().unwrap();
        assert_eq!(e.kind, StepErrorKind::NotStep);
        assert!(e.to_string().contains("line 1"));
    }

    #[test]
    fn step_string_pe_s_decodes_cyrillic_8859_5() {
        let f = StepFile::parse(wrap(r"#1=A('\PE\\S\0\S\P','\S\i');").as_bytes()).unwrap();
        let a = f.get(1).unwrap().args();
        assert_eq!(a[0].as_str(), Some("\u{410}\u{430}"));
        // the page is reset for the next string
        assert_eq!(a[1].as_str(), Some("\u{e9}"));
    }

    #[test]
    fn step_string_pb_s_decodes_central_european() {
        let f = StepFile::parse(wrap(r"#1=A('Z\PB\\S\3\S\1');").as_bytes()).unwrap();
        assert_eq!(f.get(1).unwrap().args()[0].as_str(), Some("Z\u{142}\u{105}"));
    }

    #[test]
    fn step_string_s_without_p_stays_latin1() {
        let f = StepFile::parse(wrap(r"#1=A('caf\S\i');").as_bytes()).unwrap();
        assert_eq!(f.get(1).unwrap().args()[0].as_str(), Some("caf\u{e9}"));
    }

    #[test]
    fn data_string_with_lone_backslash_imports_rest() {
        let src = wrap("#1=A('C:\\Temp\\x');\n#2=B(1);\n#3=C('end');");
        let f = StepFile::parse_tolerant(src.as_bytes()).unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f.type_of(3), Some("C"));
        assert_eq!(f.get(3).unwrap().args()[0].as_str(), Some("end"));
    }

    #[test]
    fn data_string_lone_backslash_kept_literal_and_warned() {
        let src = wrap("#1=A('C:\\Temp\\x');\n#2=B('\\Q');");
        let f = StepFile::parse_tolerant(src.as_bytes()).unwrap();
        assert_eq!(f.get(1).unwrap().args()[0].as_str(), Some("C:\\Temp\\x"));
        assert_eq!(f.get(2).unwrap().args()[0].as_str(), Some("\\Q"));
        // counted once per file, not once per string
        let w: Vec<_> = f.warnings.iter().filter(|w| w.contains("backslash")).collect();
        assert_eq!(w.len(), 1, "{:?}", f.warnings);
        assert!(w[0].contains("3 unknown"), "{w:?}");
    }

    #[test]
    fn strict_mode_still_rejects_unknown_escape() {
        let src = wrap("#1=A('C:\\Temp');");
        let e = StepFile::parse(src.as_bytes()).err().unwrap();
        assert_eq!(e.kind, StepErrorKind::BadString("escape"));
    }

    #[test]
    fn tolerant_parse_warns_on_duplicates_and_dangling() {
        let f = StepFile::parse_tolerant(
            wrap(
                "#1=A(#9);\n#2=B(1);\n#2=C(#1);",
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f.type_of(2), Some("B"));
        assert_eq!(f.warnings.len(), 2, "{:?}", f.warnings);
        assert!(f.get(9).is_none());
    }

    #[test]
    fn tolerant_parse_skips_bad_header_record_and_keeps_huge_integers() {
        let s = "ISO-10303-21;\nHEADER;\nFILE_NAME('a;b',*/x*/(''));\nFILE_SCHEMA(('IFC2X3'));\n\
                 ENDSEC;\nDATA;\n#1=A(100000000000000000001);\nENDSEC;\nEND-ISO-10303-21;\n";
        assert!(StepFile::parse(s.as_bytes()).is_err());
        let f = StepFile::parse_tolerant(s.as_bytes()).unwrap();
        assert_eq!(f.schema, ["IFC2X3"]);
        assert_eq!(f.warnings.len(), 1, "{:?}", f.warnings);
        assert_eq!(f.get(1).unwrap().args()[0], Value::Real(1e20));
    }

    #[test]
    fn truncation_never_panics() {
        let s = wrap(r"#1=A('\X2\042D\X0\',(1.,2.),.T.,#2);/*c*/#2=(B(1)C('x'));");
        assert!(StepFile::parse(s.as_bytes()).is_ok());
        for n in 0..s.trim_end().len() {
            assert!(StepFile::parse(&s.as_bytes()[..n]).is_err(), "prefix {n}");
        }
    }

    /// 100 MB generated file; budget 2 s in release.
    #[test]
    #[ignore]
    fn parse_budget_100mb() {
        let mut s = String::with_capacity(105 << 20);
        s.push_str("ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n");
        let mut id = 1u32;
        while s.len() < 100 << 20 {
            let k = id;
            s.push_str(&format!(
                "#{k}=IFCCARTESIANPOINT(({}.5,-{}.25,0.));\n#{}=IFCPOLYLINE((#{k},#{k}));\n#{}=IFCWALL('2O2Fr$t4X7Zf8NOew3FLOH',$,'\\X2\\042104420435043D0430\\X0\\ {k}',$,$,$,#{},$,.STANDARD.);\n",
                k % 1000,
                k % 777,
                k + 1,
                k + 2,
                k + 1
            ));
            id += 3;
        }
        s.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
        let t = web_time::Instant::now();
        let nl = s.bytes().filter(|b| *b == 10).count();
        println!(
            "calibration: newline scan {nl} in {:.3} s",
            t.elapsed().as_secs_f64()
        );
        let t = web_time::Instant::now();
        let f = StepFile::parse(s.as_bytes()).unwrap();
        let dt = t.elapsed().as_secs_f64();
        println!(
            "parse {} MB, {} entities: {dt:.2} s",
            s.len() >> 20,
            f.len()
        );
        assert!(dt <= 2.0, "{dt} s");
    }
}
