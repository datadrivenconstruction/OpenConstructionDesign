// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! PDF document: xref tables and streams, the /Prev chain (incremental
//! updates), object streams, recovery by scanning "N G obj" when the xref is broken.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::filters;
use super::lexer::{Dict, Lexer, Obj, Parser, Tok};
use super::ImportError;

/// Decompression limit for a single stream.
pub const STREAM_LIMIT: usize = 512 << 20;
/// Total budget of decoded bytes per import (default).
pub const DECODE_BUDGET: usize = 1 << 30;
/// Decoded stream cache: the limit for one stream and for the whole cache.
const CACHE_ITEM_MAX: usize = 16 << 20;
const CACHE_TOTAL_MAX: usize = 128 << 20;
const MAX_RESOLVE: usize = 32;

type ObjStm = Rc<Vec<(u32, Obj)>>;

#[derive(Clone, Copy, Debug)]
enum Entry {
    Free,
    At(usize),
    InStream(u32, u32),
}

pub struct Document {
    data: Rc<[u8]>,
    xref: HashMap<u32, Entry>,
    pub trailer: Dict,
    cache: RefCell<HashMap<u32, Rc<Obj>>>,
    objstm: RefCell<HashMap<u32, ObjStm>>,
    /// Table from scanning the file (built lazily on an xref miss).
    scanned: RefCell<Option<HashMap<u32, usize>>>,
    busy: RefCell<HashSet<u32>>,
    /// Remaining decoding budget; `exhausted` — the budget was exceeded.
    budget: Cell<usize>,
    exhausted: Cell<bool>,
    decoded: RefCell<HashMap<(usize, usize), Option<Rc<Vec<u8>>>>>,
    decoded_bytes: Cell<usize>,
    #[cfg(test)]
    pub decode_attempts: Cell<usize>,
}

fn find_last(h: &[u8], n: &[u8]) -> Option<usize> {
    if h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len())
        .rev()
        .find(|&i| &h[i..i + n.len()] == n)
}

fn find_from(h: &[u8], n: &[u8], from: usize) -> Option<usize> {
    if from >= h.len() {
        return None;
    }
    h[from..]
        .windows(n.len())
        .position(|w| w == n)
        .map(|p| p + from)
}

impl Document {
    pub fn parse(data: &[u8]) -> Result<Self, ImportError> {
        let head = &data[..data.len().min(1024)];
        if find_from(head, b"%PDF-", 0).is_none() {
            return Err(ImportError::NotPdf);
        }
        let mut doc = Document {
            data: Rc::from(data),
            xref: HashMap::new(),
            trailer: Dict::default(),
            cache: RefCell::new(HashMap::new()),
            objstm: RefCell::new(HashMap::new()),
            scanned: RefCell::new(None),
            busy: RefCell::new(HashSet::new()),
            budget: Cell::new(DECODE_BUDGET),
            exhausted: Cell::new(false),
            decoded: RefCell::new(HashMap::new()),
            decoded_bytes: Cell::new(0),
            #[cfg(test)]
            decode_attempts: Cell::new(0),
        };
        let ok = doc.read_xref_chain();
        if !ok || doc.trailer.get(b"Root").is_none() {
            doc.rebuild();
        }
        if doc.trailer.get(b"Encrypt").is_some_and(|e| *e != Obj::Null) {
            return Err(ImportError::Encrypted);
        }
        if doc.trailer.get(b"Root").is_none() {
            return Err(ImportError::Malformed("no document catalog".into()));
        }
        Ok(doc)
    }

    fn read_xref_chain(&mut self) -> bool {
        let tail_from = self.data.len().saturating_sub(4096);
        let Some(p) = find_last(&self.data[tail_from..], b"startxref") else {
            return false;
        };
        let mut lx = Lexer::new(&self.data, tail_from + p + 9);
        let Tok::Int(mut off) = lx.next() else {
            return false;
        };
        let mut seen = HashSet::new();
        let mut first = true;
        while off >= 0 && seen.insert(off) && seen.len() < 1000 {
            let Some(tr) = self.read_section(off as usize) else {
                return !first;
            };
            if first {
                self.trailer = tr.clone();
                first = false;
            }
            // Hybrid file: /XRefStm supplements the table of the same revision.
            if let Some(x) = tr.get(b"XRefStm").and_then(Obj::int) {
                if x >= 0 && seen.insert(x) {
                    self.read_section(x as usize);
                }
            }
            match tr.get(b"Prev").and_then(Obj::int) {
                Some(p) => off = p,
                None => break,
            }
        }
        true
    }

    /// An xref section at an offset: a table or a stream. New entries are not overridden by old ones.
    fn read_section(&mut self, off: usize) -> Option<Dict> {
        let data = self.data.clone();
        let mut lx = Lexer::new(&data, off);
        if lx.next() == Tok::Keyword(b"xref") {
            loop {
                let save = lx.pos;
                let (Tok::Int(start), Tok::Int(count)) = (lx.next(), lx.next()) else {
                    lx.pos = save;
                    break;
                };
                if start < 0 || count < 0 || start > u32::MAX as i64 {
                    return None;
                }
                for i in 0..count {
                    let (Tok::Int(o), Tok::Int(_g), Tok::Keyword(k)) =
                        (lx.next(), lx.next(), lx.next())
                    else {
                        return None;
                    };
                    let n = u32::try_from(start + i).ok()?;
                    let e = if k == b"n" && o > 0 {
                        Entry::At(o as usize)
                    } else {
                        Entry::Free
                    };
                    self.xref.entry(n).or_insert(e);
                }
            }
            if lx.next() != Tok::Keyword(b"trailer") {
                return None;
            }
            let mut p = Parser::new(&data, lx.pos, true);
            return match p.object(0) {
                Ok(Obj::Dict(d)) => Some(d),
                _ => None,
            };
        }
        let (_, obj) = self.parse_indirect_at(off)?;
        let Obj::Stream(d, raw) = obj else {
            return None;
        };
        if d.get(b"Type").and_then(Obj::name) != Some(b"XRef") {
            return None;
        }
        let body = self.decode_uncached(&d, &raw).ok()?;
        let w: Vec<usize> = d
            .get(b"W")?
            .array()?
            .iter()
            .map(|o| o.int().unwrap_or(-1))
            .map(|v| if (0..=8).contains(&v) { v as usize } else { 99 })
            .collect();
        if w.len() < 3 || w.iter().any(|&v| v > 8) {
            return None;
        }
        let size = d.get(b"Size").and_then(Obj::int).unwrap_or(0);
        let index: Vec<i64> = match d.get(b"Index").and_then(Obj::array) {
            Some(a) => a.iter().map(|o| o.int().unwrap_or(-1)).collect(),
            None => vec![0, size],
        };
        let rec = w[0] + w[1] + w[2];
        if rec == 0 {
            return Some(d);
        }
        let mut pos = 0usize;
        let field = |b: &[u8], n: usize| b.iter().take(n).fold(0u64, |a, &c| a << 8 | c as u64);
        for pair in index.chunks(2) {
            let [start, count] = pair else { break };
            if *start < 0 || *count < 0 {
                break;
            }
            for i in 0..*count {
                let Some(r) = body.get(pos..pos + rec) else {
                    return Some(d);
                };
                pos += rec;
                let ty = if w[0] == 0 { 1 } else { field(r, w[0]) };
                let f2 = field(&r[w[0]..], w[1]);
                let f3 = field(&r[w[0] + w[1]..], w[2]);
                let Ok(n) = u32::try_from(start + i) else {
                    return Some(d);
                };
                let e = match ty {
                    1 => Entry::At(f2 as usize),
                    2 => Entry::InStream(f2.min(u32::MAX as u64) as u32, f3 as u32),
                    _ => Entry::Free,
                };
                self.xref.entry(n).or_insert(e);
            }
        }
        Some(d)
    }

    /// Scans the whole file: the last "N G obj" wins; the trailer is the last `trailer`
    /// dictionary or an xref stream with /Root, otherwise the catalog is searched for.
    fn rebuild(&mut self) {
        let map = self.scan();
        self.xref = map.iter().map(|(&n, &o)| (n, Entry::At(o))).collect();
        self.cache.borrow_mut().clear();
        let data = self.data.clone();
        let mut from = data.len();
        while let Some(p) = find_last(&data[..from], b"trailer") {
            if let Ok(Obj::Dict(d)) = Parser::new(&data, p + 7, true).object(0) {
                if d.get(b"Root").is_some() {
                    self.trailer = d;
                    return;
                }
            }
            from = p;
        }
        // Object streams are known only through xref streams: read their entries too.
        let mut nums: Vec<u32> = map.keys().copied().collect();
        nums.sort_unstable();
        for &n in nums.iter().rev() {
            if let Some((_, Obj::Stream(d, _))) = self.parse_indirect_at(map[&n]) {
                if d.get(b"Type").and_then(Obj::name) == Some(b"XRef") {
                    let saved = std::mem::take(&mut self.xref);
                    self.read_section(map[&n]);
                    let streams = std::mem::replace(&mut self.xref, saved);
                    for (k, v) in streams {
                        if let Entry::InStream(..) = v {
                            self.xref.entry(k).or_insert(v);
                        }
                    }
                    if d.get(b"Root").is_some() && self.trailer.get(b"Root").is_none() {
                        self.trailer = d.clone();
                    }
                }
            }
        }
        if self.trailer.get(b"Root").is_some() {
            return;
        }
        for &n in &nums {
            if let Ok(o) = self.get(n) {
                if o.dict().and_then(|d| d.get(b"Type")).and_then(Obj::name) == Some(b"Catalog") {
                    self.trailer.0.push((b"Root".to_vec(), Obj::Ref(n, 0)));
                    return;
                }
            }
        }
    }

    fn scan(&self) -> HashMap<u32, usize> {
        let d = &self.data;
        let mut map = HashMap::new();
        let mut i = 0;
        while let Some(p) = find_from(d, b"obj", i) {
            i = p + 3;
            if d.get(p + 3)
                .is_some_and(|c| !super::lexer::is_ws(*c) && !super::lexer::is_delim(*c))
            {
                continue;
            }
            // Backwards: whitespace, generation, whitespace, number.
            let mut j = p;
            let back_ws = |j: &mut usize| {
                let s = *j;
                while *j > 0 && super::lexer::is_ws(d[*j - 1]) {
                    *j -= 1;
                }
                *j < s
            };
            let back_num = |j: &mut usize| {
                let s = *j;
                while *j > 0 && d[*j - 1].is_ascii_digit() && s - *j < 10 {
                    *j -= 1;
                }
                *j < s
            };
            if !(back_ws(&mut j) && back_num(&mut j)) {
                continue;
            }
            if !(back_ws(&mut j) && back_num(&mut j)) {
                continue;
            }
            if j > 0 && !super::lexer::is_ws(d[j - 1]) && !super::lexer::is_delim(d[j - 1]) {
                continue;
            }
            let mut lx = Lexer::new(d, j);
            if let Tok::Int(n) = lx.next() {
                if let Ok(n) = u32::try_from(n) {
                    map.insert(n, j);
                }
            }
        }
        map
    }

    /// Parses "N G obj … endobj" at an offset.
    fn parse_indirect_at(&self, off: usize) -> Option<(u32, Obj)> {
        let d = &self.data;
        let mut p = Parser::new(d, off, true);
        let (Tok::Int(n), Tok::Int(_), Tok::Keyword(b"obj")) =
            (p.lx.next(), p.lx.next(), p.lx.next())
        else {
            return None;
        };
        let n = u32::try_from(n).ok()?;
        let obj = p.object(0).ok()?;
        if let Obj::Dict(dict) = &obj {
            if p.lx.next() == Tok::Keyword(b"stream") {
                let mut s = p.lx.pos;
                if d.get(s) == Some(&b'\r') {
                    s += 1;
                }
                if d.get(s) == Some(&b'\n') {
                    s += 1;
                }
                let len = dict
                    .get(b"Length")
                    .and_then(|l| self.resolve_shallow(l))
                    .and_then(|l| l.int())
                    .filter(|&l| l >= 0)
                    .map(|l| l as usize);
                let good = len.and_then(|l| {
                    let e = s.checked_add(l)?;
                    let mut lx = Lexer::new(d, e);
                    (e <= d.len() && lx.next() == Tok::Keyword(b"endstream")).then_some(e)
                });
                let e = match good {
                    Some(e) => e,
                    None => {
                        let mut e = find_from(d, b"endstream", s).unwrap_or(d.len());
                        if e > s && d[e - 1] == b'\n' {
                            e -= 1;
                        }
                        if e > s && d[e - 1] == b'\r' {
                            e -= 1;
                        }
                        e
                    }
                };
                return Some((n, Obj::Stream(dict.clone(), Rc::from(&d[s..e.max(s)]))));
            }
        }
        Some((n, obj))
    }

    /// /Length is often indirect; resolve it without streams to avoid recursion.
    fn resolve_shallow(&self, o: &Obj) -> Option<Obj> {
        match o {
            Obj::Ref(n, _) => match self.xref.get(n) {
                Some(Entry::At(off)) => {
                    let mut p = Parser::new(&self.data, *off, true);
                    let _ = (p.lx.next(), p.lx.next(), p.lx.next());
                    p.object(0).ok()
                }
                _ => self.get(*n).ok().map(|o| (*o).clone()),
            },
            o => Some(o.clone()),
        }
    }

    pub fn get(&self, n: u32) -> Result<Rc<Obj>, ImportError> {
        if let Some(o) = self.cache.borrow().get(&n) {
            return Ok(o.clone());
        }
        if !self.busy.borrow_mut().insert(n) {
            return Ok(Rc::new(Obj::Null));
        }
        let r = self.load(n);
        self.busy.borrow_mut().remove(&n);
        let o = Rc::new(r?);
        self.cache.borrow_mut().insert(n, o.clone());
        Ok(o)
    }

    fn load(&self, n: u32) -> Result<Obj, ImportError> {
        match self.xref.get(&n).copied() {
            Some(Entry::At(off)) => {
                if let Some((m, o)) = self.parse_indirect_at(off) {
                    if m == n {
                        return Ok(o);
                    }
                }
                self.load_scanned(n)
            }
            Some(Entry::InStream(s, idx)) => {
                let objs = self.object_stream(s)?;
                if let Some((_, o)) = objs.get(idx as usize).filter(|(m, _)| *m == n) {
                    return Ok(o.clone());
                }
                Ok(objs
                    .iter()
                    .find(|(m, _)| *m == n)
                    .map_or(Obj::Null, |(_, o)| o.clone()))
            }
            Some(Entry::Free) => Ok(Obj::Null),
            None => self.load_scanned(n),
        }
    }

    fn load_scanned(&self, n: u32) -> Result<Obj, ImportError> {
        if self.scanned.borrow().is_none() {
            let m = self.scan();
            *self.scanned.borrow_mut() = Some(m);
        }
        let off = self
            .scanned
            .borrow()
            .as_ref()
            .and_then(|m| m.get(&n).copied());
        Ok(off
            .and_then(|o| self.parse_indirect_at(o))
            .map_or(Obj::Null, |(_, o)| o))
    }

    fn object_stream(&self, s: u32) -> Result<Rc<Vec<(u32, Obj)>>, ImportError> {
        if let Some(v) = self.objstm.borrow().get(&s) {
            return Ok(v.clone());
        }
        let o = self.get(s)?;
        let mut out = vec![];
        if let Obj::Stream(d, raw) = &*o {
            // A failure is cached too: otherwise every lookup would re-decompress a broken stream.
            let body = match self.decode_stream(d, raw) {
                Ok(b) => b,
                Err(e) => {
                    self.objstm.borrow_mut().insert(s, Rc::new(vec![]));
                    return Err(e);
                }
            };
            let count = d
                .get(b"N")
                .and_then(Obj::int)
                .unwrap_or(0)
                .clamp(0, 1 << 20);
            let first = d.get(b"First").and_then(Obj::int).unwrap_or(0).max(0) as usize;
            let mut lx = Lexer::new(&body, 0);
            let mut hdr = vec![];
            for _ in 0..count {
                let (Tok::Int(num), Tok::Int(off)) = (lx.next(), lx.next()) else {
                    break;
                };
                hdr.push((num, off));
            }
            for (num, off) in hdr {
                let (Ok(num), Ok(off)) = (u32::try_from(num), usize::try_from(off)) else {
                    continue;
                };
                let Some(pos) = first.checked_add(off).filter(|p| *p <= body.len()) else {
                    continue;
                };
                let obj = Parser::new(&body, pos, true).object(0).unwrap_or(Obj::Null);
                out.push((num, obj));
            }
        }
        let v = Rc::new(out);
        self.objstm.borrow_mut().insert(s, v.clone());
        Ok(v)
    }

    /// Dereferences a chain of references (length-limited).
    pub fn resolve(&self, o: &Obj) -> Rc<Obj> {
        let mut cur = match o {
            Obj::Ref(..) => None,
            o => return Rc::new(o.clone()),
        };
        let mut r = o.clone();
        for _ in 0..MAX_RESOLVE {
            let Obj::Ref(n, _) = r else { break };
            let got = self.get(n).unwrap_or_else(|_| Rc::new(Obj::Null));
            r = (*got).clone_shallow_ref();
            cur = Some(got);
        }
        match cur {
            Some(c) if !matches!(*c, Obj::Ref(..)) => c,
            _ => Rc::new(Obj::Null),
        }
    }

    /// A dictionary key's value, dereferenced.
    pub fn get_in(&self, d: &Dict, k: &[u8]) -> Rc<Obj> {
        match d.get(k) {
            Some(o) => self.resolve(o),
            None => Rc::new(Obj::Null),
        }
    }

    #[cfg(test)]
    pub fn test_add_instream(&mut self, first: u32, count: u32, stream: u32) {
        for i in 0..count {
            self.xref.insert(first + i, Entry::InStream(stream, i));
        }
    }

    pub fn set_decode_budget(&self, bytes: usize) {
        self.budget.set(bytes);
        self.exhausted.set(false);
    }

    pub fn budget_exhausted(&self) -> bool {
        self.exhausted.get()
    }

    fn over_budget() -> ImportError {
        ImportError::Filter("total decoded size budget exceeded".into())
    }

    /// Charges `n` bytes to the budget (for data copied on top of decoding).
    pub fn charge(&self, n: usize) -> Result<(), ImportError> {
        let left = self.budget.get();
        if self.exhausted.get() || n > left {
            self.exhausted.set(true);
            return Err(Self::over_budget());
        }
        self.budget.set(left - n);
        Ok(())
    }

    /// Decoding with a cache keyed by stream data position (objects live in the document cache).
    pub fn decode_stream(&self, d: &Dict, raw: &Rc<[u8]>) -> Result<Rc<Vec<u8>>, ImportError> {
        let key = (raw.as_ptr() as usize, raw.len());
        if let Some(hit) = self.decoded.borrow().get(&key) {
            return hit
                .clone()
                .ok_or_else(|| ImportError::Filter("stream failed to decode".into()));
        }
        match self.decode_uncached(d, raw) {
            Ok(v) => {
                let v = Rc::new(v);
                let total = self.decoded_bytes.get();
                if v.len() <= CACHE_ITEM_MAX && total + v.len() <= CACHE_TOTAL_MAX {
                    self.decoded_bytes.set(total + v.len());
                    self.decoded.borrow_mut().insert(key, Some(v.clone()));
                }
                Ok(v)
            }
            Err(e) => {
                if !self.exhausted.get() {
                    self.decoded.borrow_mut().insert(key, None);
                }
                Err(e)
            }
        }
    }

    fn decode_uncached(&self, d: &Dict, raw: &[u8]) -> Result<Vec<u8>, ImportError> {
        #[cfg(test)]
        self.decode_attempts.set(self.decode_attempts.get() + 1);
        if self.exhausted.get() {
            return Err(Self::over_budget());
        }
        let left = self.budget.get();
        let limit = STREAM_LIMIT.min(left);
        let names = |o: &Obj| -> Vec<Vec<u8>> {
            match o {
                Obj::Name(n) => vec![n.clone()],
                Obj::Array(a) => a
                    .iter()
                    .filter_map(|x| self.resolve(x).name().map(<[u8]>::to_vec))
                    .collect(),
                _ => vec![],
            }
        };
        let f = self.get_in(d, b"Filter");
        let filters = names(&f);
        let p = self.get_in(d, b"DecodeParms");
        let parms: Vec<Option<Dict>> = match &*p {
            Obj::Dict(x) => vec![Some(x.clone())],
            Obj::Array(a) => a.iter().map(|x| self.resolve(x).dict().cloned()).collect(),
            _ => vec![],
        };
        match filters::decode(raw, &filters, &parms, limit) {
            Ok(v) => {
                self.charge(v.len())?;
                Ok(v)
            }
            Err(e) => {
                if limit < STREAM_LIMIT && e.to_string().contains("size limit") {
                    self.exhausted.set(true);
                    return Err(Self::over_budget());
                }
                Err(e)
            }
        }
    }
}

impl Obj {
    /// A reference chain needs only the reference object itself; nothing else is copied.
    fn clone_shallow_ref(&self) -> Obj {
        match self {
            Obj::Ref(n, g) => Obj::Ref(*n, *g),
            _ => Obj::Null,
        }
    }
}
