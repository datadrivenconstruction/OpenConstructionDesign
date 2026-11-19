// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Streaming STEP open for big files: the file is read in fixed chunks and split into
//! statements (`;` outside strings, binaries and comments, state carried across chunk
//! borders); each statement is validated by the same parser as [`StepFile::parse`], only a
//! compact slot (offset, length, type, line) is kept. Arguments are decoded on demand from a
//! positional read through a small page cache: [`StepFile::get`] pins what it hands out,
//! [`StepFile::leaf`] goes through a bounded cache, so a 500 MB file never sits in memory as
//! a whole.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use super::{latin, Entity, IdMap, StepError, StepErrorKind, StepFile, Value, P};
use crate::job::Progress;

#[derive(Debug, Clone, Copy)]
pub struct StreamOptions {
    /// Bytes per read of the indexing pass.
    pub chunk: usize,
    /// Page of the decode cache and number of pages kept.
    pub page: usize,
    pub pages: usize,
    /// Source bytes of leaf arguments per cache generation (two generations are kept).
    pub leaves: usize,
    /// Longest statement accepted (an unterminated string must not swallow the file).
    pub max_statement: usize,
    /// Duplicate ids (first kept), dangling references and broken header records become
    /// warnings ([`StepFile::warnings`]) instead of errors; syntax errors in DATA fail.
    pub tolerant: bool,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            chunk: 4 << 20,
            page: 1 << 20,
            pages: 64,
            leaves: 16 << 20,
            max_statement: 64 << 20,
            tolerant: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpenError {
    Io(String),
    Step(StepError),
    Cancelled,
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            OpenError::Io(e) => write!(f, "read: {e}"),
            OpenError::Step(e) => fmt::Display::fmt(e, f),
            OpenError::Cancelled => f.write_str(crate::job::CANCELLED),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<StepError> for OpenError {
    fn from(e: StepError) -> Self {
        OpenError::Step(e)
    }
}

/// Decoded entities between progress reports.
const REPORT_EVERY: u64 = 4096;
/// Index entries reserved up front; the map grows past it (a guess from the file size would
/// commit gigabytes before the first entity on a multi-GB file).
const RESERVE: usize = 1 << 22;

/// Index entry of a streamed entity: 16 bytes (48-bit offset, length, type, line).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Slot {
    off: u32,
    off_hi: u16,
    ty: u16,
    len: u32,
    line: u32,
}

impl Slot {
    fn off(&self) -> u64 {
        self.off as u64 | (self.off_hi as u64) << 32
    }
    pub(super) fn ty(&self) -> usize {
        self.ty as usize
    }
}

enum Src {
    File(File),
    /// Tests: the same lazy path over bytes in memory.
    #[cfg_attr(not(test), allow(dead_code))]
    Mem(Vec<u8>),
}

/// Leaf arguments in two generations: the older one is dropped when the newer fills, a hit
/// in the older one moves the entry forward (approximate LRU, O(1) per access).
#[derive(Default)]
struct Leaves {
    hot: IdMap<(Arc<[Value]>, u32)>,
    cold: IdMap<(Arc<[Value]>, u32)>,
    hot_bytes: usize,
    cap: usize,
}

impl Leaves {
    fn get(&mut self, id: u32) -> Option<Arc<[Value]>> {
        if let Some(a) = self.hot.get(&id) {
            return Some(a.0.clone());
        }
        let (a, w) = self.cold.remove(&id)?;
        self.put(id, a.clone(), w);
        Some(a)
    }

    fn put(&mut self, id: u32, a: Arc<[Value]>, w: u32) {
        if self.hot_bytes + w as usize > self.cap && !self.hot.is_empty() {
            self.cold = std::mem::take(&mut self.hot);
            self.hot_bytes = 0;
        }
        self.hot_bytes += w as usize;
        self.hot.insert(id, (a, w));
    }

    fn len(&self) -> usize {
        self.hot.len() + self.cold.len()
    }
}

/// File source of a streamed [`StepFile`].
pub(in crate::ifc) struct Lazy {
    src: Src,
    len: u64,
    page: usize,
    pages: usize,
    /// page number → (bytes, last use).
    cache: RefCell<HashMap<u64, (Box<[u8]>, u64)>>,
    clock: Cell<u64>,
    decoded: Cell<u64>,
    /// Spans that no longer read or parse (the file changed after indexing).
    failed: Cell<u64>,
    ctl: Option<Arc<Progress>>,
    /// Entities handed out by [`StepFile::get`]; boxed so their address survives map growth.
    pinned: RefCell<IdMap<Box<Entity>>>,
    leaves: RefCell<Leaves>,
}

fn read_at(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.read_exact_at(buf, off)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut n = 0;
        while n < buf.len() {
            let k = f.seek_read(&mut buf[n..], off + n as u64)?;
            if k == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            n += k;
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        use std::io::{Seek, SeekFrom};
        let mut r: &File = f;
        r.seek(SeekFrom::Start(off))?;
        r.read_exact(buf)
    }
}

impl Lazy {
    fn new(src: Src, len: u64, opt: &StreamOptions, ctl: Option<Arc<Progress>>) -> Self {
        Lazy {
            src,
            len,
            page: opt.page.max(4096),
            pages: opt.pages,
            cache: RefCell::new(HashMap::new()),
            clock: Cell::new(0),
            decoded: Cell::new(0),
            failed: Cell::new(0),
            ctl,
            pinned: RefCell::new(IdMap::default()),
            leaves: RefCell::new(Leaves {
                cap: opt.leaves.max(1),
                ..Default::default()
            }),
        }
    }

    /// Bytes `[a, b)` of the file through the page cache; None on an I/O error.
    fn bytes(&self, a: u64, b: u64) -> Option<Vec<u8>> {
        let file = match &self.src {
            Src::Mem(m) => return m.get(a as usize..b as usize).map(<[u8]>::to_vec),
            Src::File(f) => f,
        };
        let page = self.page as u64;
        let mut out = Vec::with_capacity((b - a) as usize);
        let mut cache = self.cache.borrow_mut();
        let mut pos = a;
        while pos < b {
            let no = pos / page;
            let t = self.clock.get() + 1;
            self.clock.set(t);
            if !cache.contains_key(&no) {
                if cache.len() >= self.pages.max(1) {
                    if let Some(old) = cache.iter().min_by_key(|(_, v)| v.1).map(|(k, _)| *k) {
                        cache.remove(&old);
                    }
                }
                let start = no * page;
                let n = (self.len.saturating_sub(start)).min(page) as usize;
                let mut buf = vec![0u8; n].into_boxed_slice();
                read_at(file, &mut buf, start).ok()?;
                cache.insert(no, (buf, t));
            }
            let e = cache.get_mut(&no)?;
            e.1 = t;
            let from = (pos - no * page) as usize;
            let to = ((b - no * page) as usize).min(e.0.len());
            if from >= to {
                return None;
            }
            out.extend_from_slice(&e.0[from..to]);
            pos = no * page + to as u64;
        }
        Some(out)
    }

    /// Typed arguments of a slot; None once the job is cancelled (callers stop early) or when
    /// the span no longer reads or parses (counted in `failed`).
    fn decode(&self, s: &Slot) -> Option<Vec<Value>> {
        let n = self.decoded.get() + 1;
        self.decoded.set(n);
        if let Some(c) = &self.ctl {
            if c.is_cancelled() {
                return None;
            }
            if n % REPORT_EVERY == 0 {
                c.set(n);
            }
        }
        let v = self.bytes(s.off(), s.off() + s.len as u64).and_then(|b| {
            let mut p = P::new(&b, true);
            p.lenient = true;
            p.line = s.line as usize;
            let v = p.body().ok().map(|x| x.1);
            v
        });
        if v.is_none() {
            self.failed.set(self.failed.get() + 1);
        }
        v
    }

    fn pinned(&self, id: u32) -> Option<&Entity> {
        let m = self.pinned.borrow();
        let p: *const Entity = &**m.get(&id)?;
        // SAFETY: a pinned box is only ever inserted, never removed or replaced while `self`
        // lives; growing the map moves the box pointer, not the entity it points to.
        Some(unsafe { &*p })
    }

    pub(super) fn entity(&self, id: u32, s: &Slot) -> Option<&Entity> {
        if let Some(e) = self.pinned(id) {
            return Some(e);
        }
        let hit = self.leaves.borrow_mut().get(id);
        let args = match hit {
            Some(a) => a.to_vec(),
            None => self.decode(s)?,
        };
        let e = Box::new(Entity {
            ty: s.ty as u32,
            line: s.line,
            span: (0, 0),
            args: OnceCell::from(args),
        });
        let mut m = self.pinned.borrow_mut();
        let p: *const Entity = &**m.entry(id).or_insert(e);
        // SAFETY: as in `pinned`.
        Some(unsafe { &*p })
    }

    pub(super) fn leaf(&self, id: u32, s: &Slot) -> Option<super::Args<'_>> {
        if let Some(e) = self.pinned(id) {
            return Some(super::Args::Borrowed(e.args()));
        }
        let hit = self.leaves.borrow_mut().get(id);
        let a = match hit {
            Some(a) => a,
            None => {
                let a: Arc<[Value]> = self.decode(s)?.into();
                self.leaves.borrow_mut().put(id, a.clone(), s.len);
                a
            }
        };
        Some(super::Args::Shared(a))
    }
}

/// Parser scratch buffers reused across statements (a fresh `P` per entity would allocate).
#[derive(Default)]
struct Bufs {
    refs: Vec<u32>,
    stack: Vec<Value>,
    sbuf: String,
    rbuf: Vec<u8>,
    ubuf: Vec<u16>,
}

impl Bufs {
    fn parser<'a>(&mut self, b: &'a [u8], line: usize, build: bool) -> P<'a> {
        let mut stack = std::mem::take(&mut self.stack);
        if stack.capacity() == 0 {
            stack.reserve(256);
        }
        P {
            lenient: false,
            lone: 0,
            lone_line: 0,
            b,
            i: 0,
            line,
            ent: None,
            build,
            refs: std::mem::take(&mut self.refs),
            stack,
            sbuf: std::mem::take(&mut self.sbuf),
            rbuf: std::mem::take(&mut self.rbuf),
            ubuf: std::mem::take(&mut self.ubuf),
        }
    }

    fn restore(&mut self, p: P) {
        self.refs = p.refs;
        self.stack = p.stack;
        self.sbuf = p.sbuf;
        self.rbuf = p.rbuf;
        self.ubuf = p.ubuf;
    }
}

/// Splitter state. Inside a string a `\` escape is consumed the way [`P`] reads it (`\\`,
/// `\S\c`, `\Px\`, `\X\hh`, `\X2\` / `\X4\` / `\X0\`), so an escaped quote (`\S\'`, `\P'\`)
/// does not end the string; `''` is two adjacent strings to the splitter, which is the same.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Lex {
    Normal,
    /// `/` seen in Normal: a comment starts if `*` follows.
    Slash,
    Str,
    /// `\` in a string.
    Esc,
    /// `\S`: expects `\`.
    EscS,
    /// `\S\`: the next byte is literal.
    EscLit,
    /// `\P`: the next byte is the code page letter.
    EscP,
    /// `\Px`: expects `\`.
    EscPEnd,
    /// `\X`: `\` (then two hex digits), or `2` / `4` / `0` then `\`.
    EscX,
    EscXEnd,
    Bin,
    Comment,
    /// `*` seen inside a comment.
    Star,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Magic,
    HeaderKw,
    Header,
    Section,
    Data,
    Done,
}

/// Nothing but blanks and comments may follow a statement's `;`: anything else means two
/// statements were split as one, and the second must not vanish silently.
fn end(p: &mut P) -> Result<(), StepError> {
    p.ws()?;
    match p.peek() {
        None => Ok(()),
        Some(_) => Err(p.unexpected()),
    }
}

struct Builder {
    f: StepFile,
    type_ix: HashMap<Vec<u8>, u32>,
    /// References whose target was not yet defined when seen.
    pending: Vec<(u32, u32)>,
    phase: Phase,
    bufs: Bufs,
    tolerant: bool,
    /// Unknown string escapes kept literally, and the line of the first.
    lone: u32,
    lone_line: usize,
}

impl Builder {
    fn new(tolerant: bool) -> Self {
        Builder {
            lone: 0,
            lone_line: 0,
            f: StepFile::default(),
            type_ix: HashMap::new(),
            pending: vec![],
            phase: Phase::Magic,
            bufs: Bufs::default(),
            tolerant,
        }
    }

    /// One statement (ends with `;`, or the unterminated tail at end of file).
    fn statement(&mut self, s: &[u8], off: u64, line: usize) -> Result<(), StepError> {
        let mut p = self.bufs.parser(s, line, false);
        p.lenient = self.tolerant;
        let r = self.statement_in(&mut p, off);
        if p.lone > 0 {
            if self.lone == 0 {
                self.lone_line = p.lone_line;
            }
            self.lone += p.lone;
        }
        self.bufs.restore(p);
        r
    }

    fn statement_in(&mut self, p: &mut P, off: u64) -> Result<(), StepError> {
        p.ws()?;
        match self.phase {
            Phase::Magic => {
                p.word("ISO-10303-21")?;
                p.ws()?;
                p.eat(b';')?;
                end(p)?;
                self.phase = Phase::HeaderKw;
            }
            Phase::HeaderKw => {
                p.word("HEADER")?;
                p.ws()?;
                p.eat(b';')?;
                end(p)?;
                self.phase = Phase::Header;
            }
            Phase::Header => {
                let k = p.keyword()?;
                if k == b"ENDSEC" {
                    p.ws()?;
                    p.eat(b';')?;
                    end(p)?;
                    self.phase = Phase::Section;
                    return Ok(());
                }
                let r = (|| -> Result<Vec<Value>, StepError> {
                    p.ws()?;
                    p.build = true;
                    let args = p.params(0)?;
                    p.build = false;
                    p.ws()?;
                    p.eat(b';')?;
                    end(p)?;
                    Ok(args)
                })();
                p.build = false;
                match r {
                    Ok(args) => {
                        if k == b"FILE_SCHEMA" {
                            if let Some(Value::List(l)) = args.first() {
                                self.f.schema = l
                                    .iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect();
                            }
                        }
                    }
                    // a malformed header record carries no geometry: skip it, keep the file
                    Err(e) if self.tolerant && k != b"FILE_SCHEMA" => {
                        self.f.warn(format!(
                            "header {}: {e}; skipped",
                            String::from_utf8_lossy(k)
                        ));
                    }
                    Err(e) => return Err(e),
                }
            }
            Phase::Section => {
                let k = p.keyword()?;
                if k == b"END-ISO-10303-21" {
                    p.ws()?;
                    p.eat(b';')?;
                    self.phase = Phase::Done;
                    return Ok(());
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
                end(p)?;
                self.phase = Phase::Data;
            }
            Phase::Data => match p.peek() {
                Some(b'#') => self.entity(p, off)?,
                Some(_) => {
                    p.word("ENDSEC")?;
                    p.ws()?;
                    p.eat(b';')?;
                    end(p)?;
                    self.phase = Phase::Section;
                }
                None => return Err(p.err(StepErrorKind::Eof)),
            },
            Phase::Done => {}
        }
        Ok(())
    }

    fn entity(&mut self, p: &mut P, off: u64) -> Result<(), StepError> {
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
        let end_at = p.i;
        p.ws()?;
        p.eat(b';')?;
        end(p)?;
        if self.f.slots.contains_key(&id) {
            if !self.tolerant {
                return Err(StepError {
                    line,
                    entity: Some(id),
                    kind: StepErrorKind::DuplicateId,
                });
            }
            p.ent = None;
            self.f.warn(format!("line {line}: duplicate #{id} ignored"));
            return Ok(());
        }
        let ty = match self.type_ix.get(name) {
            Some(t) => *t,
            None => {
                let t = self.f.types.len() as u32;
                if t > u16::MAX as u32 {
                    return Err(p.err(StepErrorKind::Expected("at most 65536 entity types")));
                }
                self.f.types.push(latin(name));
                self.type_ix.insert(name.to_vec(), t);
                t
            }
        };
        let at = off + start as u64;
        if at >> 48 != 0 {
            return Err(p.err(StepErrorKind::Expected("a file below 256 TB")));
        }
        p.ent = None;
        for r in &p.refs {
            if *r != id && !self.f.slots.contains_key(r) {
                self.pending.push((id, *r));
            }
        }
        self.f.slots.insert(
            id,
            Slot {
                off: at as u32,
                off_hi: (at >> 32) as u16,
                ty: ty as u16,
                len: (end_at - start) as u32,
                line: line as u32,
            },
        );
        self.f.order.push(id);
        Ok(())
    }

    fn finish(mut self) -> Result<StepFile, StepError> {
        let pending = std::mem::take(&mut self.pending);
        let mut dangling = 0usize;
        for (from, to) in pending {
            if self.f.slots.contains_key(&to) {
                continue;
            }
            let line = self.f.slots.get(&from).map_or(0, |s| s.line as usize);
            if !self.tolerant {
                return Err(StepError {
                    line,
                    entity: Some(from),
                    kind: StepErrorKind::MissingRef(to),
                });
            }
            dangling += 1;
            if dangling <= 20 {
                self.f
                    .warn(format!("line {line}: #{from} refers to missing #{to}"));
            }
        }
        if dangling > 20 {
            self.f
                .warn(format!("{} more dangling references", dangling - 20));
        }
        if self.lone > 0 {
            self.f.warn(super::lone_warning(self.lone, self.lone_line));
        }
        Ok(self.f)
    }
}

/// Statement splitter; carries the lexer state and the unfinished statement across chunks.
struct Splitter {
    lex: Lex,
    buf: Vec<u8>,
    /// File offset and line of the first byte of the current statement.
    off: u64,
    line: usize,
    max: usize,
}

fn newlines(s: &[u8]) -> usize {
    s.iter().filter(|c| **c == b'\n').count()
}

impl Splitter {
    fn new(max: usize) -> Self {
        Self {
            lex: Lex::Normal,
            buf: vec![],
            off: 0,
            line: 1,
            max: max.max(1),
        }
    }

    /// Feeds one chunk starting at file offset `at`; `emit(statement, offset, line)` for each
    /// complete statement. Stops early once `emit` returns `Ok(true)`.
    fn feed(
        &mut self,
        chunk: &[u8],
        at: u64,
        emit: &mut dyn FnMut(&[u8], u64, usize) -> Result<bool, StepError>,
    ) -> Result<bool, StepError> {
        let mut start = 0usize;
        let mut i = 0usize;
        while i < chunk.len() {
            let c = chunk[i];
            match self.lex {
                Lex::Normal => {
                    let Some(k) = chunk[i..]
                        .iter()
                        .position(|c| matches!(c, b'\'' | b'"' | b'/' | b';'))
                    else {
                        break;
                    };
                    i += k;
                    match chunk[i] {
                        b'\'' => self.lex = Lex::Str,
                        b'"' => self.lex = Lex::Bin,
                        b'/' => self.lex = Lex::Slash,
                        _ => {
                            let stop = if self.buf.is_empty() {
                                let s = &chunk[start..=i];
                                let stop = emit(s, self.off, self.line)?;
                                self.line += newlines(s);
                                stop
                            } else {
                                self.buf.extend_from_slice(&chunk[start..=i]);
                                let s = std::mem::take(&mut self.buf);
                                let stop = emit(&s, self.off, self.line)?;
                                self.line += newlines(&s);
                                self.buf = s;
                                self.buf.clear();
                                stop
                            };
                            start = i + 1;
                            self.off = at + start as u64;
                            if stop {
                                return Ok(true);
                            }
                        }
                    }
                }
                Lex::Slash => {
                    if c == b'*' {
                        self.lex = Lex::Comment;
                    } else {
                        // not a comment: the byte is ordinary input
                        self.lex = Lex::Normal;
                        continue;
                    }
                }
                Lex::Str => match chunk[i..].iter().position(|c| matches!(c, b'\'' | b'\\')) {
                    Some(k) => {
                        i += k;
                        self.lex = if chunk[i] == b'\'' {
                            Lex::Normal
                        } else {
                            Lex::Esc
                        };
                    }
                    None => break,
                },
                Lex::Esc => {
                    self.lex = match c {
                        b'\\' => Lex::Str,
                        b'S' => Lex::EscS,
                        b'P' => Lex::EscP,
                        b'X' => Lex::EscX,
                        _ => {
                            // not an escape the parser accepts: it fails on it; keep scanning
                            self.lex = Lex::Str;
                            continue;
                        }
                    };
                }
                Lex::EscS | Lex::EscPEnd | Lex::EscXEnd => {
                    if c == b'\\' {
                        self.lex = if self.lex == Lex::EscS {
                            Lex::EscLit
                        } else {
                            Lex::Str
                        };
                    } else {
                        self.lex = Lex::Str;
                        continue;
                    }
                }
                Lex::EscLit => self.lex = Lex::Str,
                Lex::EscP => self.lex = Lex::EscPEnd,
                Lex::EscX => {
                    self.lex = match c {
                        b'\\' => Lex::Str,
                        b'2' | b'4' | b'0' => Lex::EscXEnd,
                        _ => {
                            self.lex = Lex::Str;
                            continue;
                        }
                    };
                }
                Lex::Bin => match chunk[i..].iter().position(|c| *c == b'"') {
                    Some(k) => {
                        i += k;
                        self.lex = Lex::Normal;
                    }
                    None => break,
                },
                Lex::Comment => match chunk[i..].iter().position(|c| *c == b'*') {
                    Some(k) => {
                        i += k;
                        self.lex = Lex::Star;
                    }
                    None => break,
                },
                Lex::Star => {
                    self.lex = match c {
                        b'/' => Lex::Normal,
                        b'*' => Lex::Star,
                        _ => Lex::Comment,
                    };
                }
            }
            i += 1;
        }
        self.buf.extend_from_slice(&chunk[start..]);
        if self.buf.len() > self.max {
            return Err(StepError {
                line: self.line,
                entity: None,
                kind: StepErrorKind::Expected("end of statement within the length limit"),
            });
        }
        Ok(false)
    }
}

fn scan(
    r: &mut dyn Read,
    len: u64,
    opt: &StreamOptions,
    ctl: Option<&Progress>,
) -> Result<StepFile, OpenError> {
    let mut b = Builder::new(opt.tolerant);
    let guess = ((len / 64) as usize).min(RESERVE);
    b.f.slots.reserve(guess);
    b.f.order.reserve(guess);
    if let Some(c) = ctl {
        c.set_bytes(true);
        c.set_total(len);
    }
    let mut sp = Splitter::new(opt.max_statement);
    let mut chunk = vec![0u8; opt.chunk.max(1)];
    let mut at = 0u64;
    let mut done = false;
    while !done {
        if ctl.is_some_and(|c| c.is_cancelled()) {
            return Err(OpenError::Cancelled);
        }
        let n = match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(OpenError::Io(e.to_string())),
        };
        done = sp.feed(&chunk[..n], at, &mut |s, off, line| {
            b.statement(s, off, line)?;
            Ok(b.phase == Phase::Done)
        })?;
        at += n as u64;
        if let Some(c) = ctl {
            c.set(at);
        }
    }
    if b.phase != Phase::Done {
        // unterminated tail (or nothing): the parser names the error at its line
        let tail = std::mem::take(&mut sp.buf);
        b.statement(&tail, sp.off, sp.line)?;
        if b.phase != Phase::Done {
            return Err(StepError {
                line: sp.line + newlines(&tail),
                entity: None,
                kind: StepErrorKind::Eof,
            }
            .into());
        }
    }
    Ok(b.finish()?)
}

impl StepFile {
    /// Streaming open with default options (strict, like [`StepFile::parse`]).
    pub fn open(path: &Path, ctl: Option<Arc<Progress>>) -> Result<Self, OpenError> {
        Self::open_with(path, &StreamOptions::default(), ctl)
    }

    /// Indexes the file chunk by chunk (progress: bytes read; cancel checked per chunk),
    /// keeps the file open for lazy decoding (progress: entities decoded).
    pub fn open_with(
        path: &Path,
        opt: &StreamOptions,
        ctl: Option<Arc<Progress>>,
    ) -> Result<Self, OpenError> {
        let io = |e: std::io::Error| OpenError::Io(e.to_string());
        let mut file = File::open(path).map_err(io)?;
        let len = file.metadata().map_err(io)?.len();
        let mut f = scan(&mut file, len, opt, ctl.as_deref())?;
        if let Some(c) = &ctl {
            c.set_bytes(false);
            c.set_total(f.len() as u64);
            c.set(0);
        }
        f.lazy = Some(Lazy::new(Src::File(file), len, opt, ctl));
        Ok(f)
    }

    /// Bytes of the decode page cache in use (streamed files; 0 otherwise).
    pub fn cached_bytes(&self) -> usize {
        self.lazy
            .as_ref()
            .map_or(0, |l| l.cache.borrow().values().map(|v| v.0.len()).sum())
    }

    /// Entities of a streamed file whose span no longer reads or parses: the file changed on
    /// disk after indexing, and those entities looked missing to the caller.
    pub fn decode_failures(&self) -> u64 {
        self.lazy.as_ref().map_or(0, |l| l.failed.get())
    }

    /// (entities pinned by `get`, leaf entries cached) of a streamed file.
    pub fn decoded_counts(&self) -> (usize, usize) {
        self.lazy.as_ref().map_or((0, 0), |l| {
            (l.pinned.borrow().len(), l.leaves.borrow().len())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(data: &str) -> String {
        format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('a;b'),'2;1');/* x; */\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{data}\nENDSEC;\nEND-ISO-10303-21;\ntrailing junk"
        )
    }

    fn opts(chunk: usize, tolerant: bool) -> StreamOptions {
        StreamOptions {
            chunk,
            tolerant,
            ..Default::default()
        }
    }

    /// In-memory scan with a given chunk size; decoded lazily from the same bytes.
    fn scan_with(s: &[u8], opt: &StreamOptions) -> Result<StepFile, OpenError> {
        let mut f = scan(&mut &s[..], s.len() as u64, opt, None)?;
        f.lazy = Some(Lazy::new(Src::Mem(s.to_vec()), s.len() as u64, opt, None));
        Ok(f)
    }

    fn scan_mem(s: &[u8], chunk: usize, tolerant: bool) -> Result<StepFile, OpenError> {
        scan_with(s, &opts(chunk, tolerant))
    }

    fn same(a: &StepFile, b: &StepFile) {
        assert_eq!(a.schema, b.schema);
        assert_eq!(a.len(), b.len());
        let ia: Vec<(u32, String)> = a.iter().map(|(i, t)| (i, t.to_string())).collect();
        let ib: Vec<(u32, String)> = b.iter().map(|(i, t)| (i, t.to_string())).collect();
        assert_eq!(ia, ib);
        for (id, _) in ia {
            let la = a.leaf(id).unwrap();
            let lb = b.leaf(id).unwrap();
            assert_eq!(&*la, &*lb, "#{id} leaf");
            let (x, y) = (a.get(id).unwrap(), b.get(id).unwrap());
            assert_eq!(x.line, y.line, "#{id}");
            assert_eq!(x.args(), y.args(), "#{id}");
        }
    }

    const BODY: &str =
        "#1=A($,*,12,-1.5E-3,2.,'it''s;',.T.,#2,(1,(2,#2)),IFCLABEL('x'),\"0FF\");\n\
        /* comment ; with ' quote */#2=B('\\X2\\042D044204300436\\X0\\');\n\
        #3=(C(1)D('x;y'))/**/;\n#10=E(#3,#1,'multi\nline');#11=F(/*;*/#10);\n#4=G(#99);#99=H();";

    /// Every escape the parser reads, with quotes and `;` right after them.
    const ESCAPES: &str = "#1=A('\\S\\' 34;',$);\n#2=B('M\\S\\'unchen;x','\\S\\\\;');\n\
        #3=C('\\X\\E9;\\X\\27','\\X2\\00E4\\X0\\\\S\\';','\\X4\\0001F600\\X0\\');\n\
        #4=D('\\PA\\;','\\P'\\;',';''',' \\\\;');\n\
        #5=E('/*;',#1,'*/');/* '; \\S\\' */#6=F(#5);";

    #[test]
    fn every_chunk_size_gives_the_same_index() {
        for body in [BODY, ESCAPES] {
            let s = wrap(body);
            let full = StepFile::parse(s.as_bytes()).unwrap();
            for chunk in (1..=48).chain([63, 64, 65, 127, 4096]) {
                let f = scan_mem(s.as_bytes(), chunk, false).unwrap();
                same(&full, &f);
            }
        }
    }

    #[test]
    fn escaped_quote_does_not_merge_statements() {
        let s = wrap(ESCAPES);
        let f = scan_mem(s.as_bytes(), 3, false).unwrap();
        assert_eq!(f.len(), 6);
        assert_eq!(f.get(1).unwrap().args()[0].as_str(), Some("§ 34;"));
        assert_eq!(f.get(2).unwrap().args()[0].as_str(), Some("M§unchen;x"));
        assert_eq!(f.type_of(6), Some("F"));
    }

    #[test]
    fn text_after_a_statement_end_is_an_error() {
        let mut b = Builder::new(false);
        for st in [
            "ISO-10303-21;",
            "HEADER;",
            "FILE_SCHEMA(('IFC4'));",
            "ENDSEC;",
            "DATA;",
        ] {
            b.statement(st.as_bytes(), 0, 1).unwrap();
        }
        // what a desynchronised split would hand over: two entities in one statement
        let e = b.statement(b"#1=A(1);#2=B(2);", 0, 1).unwrap_err();
        assert_eq!(e.kind, StepErrorKind::Unexpected('#'));
        assert!(b.f.slots.is_empty());
        b.statement(b"/* c */ #1=A(1); /* tail */", 0, 1).unwrap();
        assert_eq!(b.f.order, [1]);
    }

    #[test]
    fn errors_match_the_in_memory_parser() {
        for data in [
            "#1=A(1);\n#2=B(1.2.3);",
            "#1=A();\n#1=A();",
            "#1=A(#9);",
            "#1=A('\\X2\\04\\X0\\');",
            "#1=A('\\Q');",
            "#1=A(1E999);",
            "#1 A(1);",
        ] {
            let s = wrap(data);
            let want = StepFile::parse(s.as_bytes()).err().expect(data);
            for chunk in [1, 3, 7, 4096] {
                match scan_mem(s.as_bytes(), chunk, false) {
                    Err(OpenError::Step(e)) => assert_eq!(e, want, "{data} @ {chunk}"),
                    other => panic!("{data} @ {chunk}: {:?}", other.err()),
                }
            }
        }
        let e = scan_mem(b"hello", 2, false).err().unwrap();
        assert!(matches!(
            e,
            OpenError::Step(StepError {
                kind: StepErrorKind::NotStep,
                ..
            })
        ));
    }

    #[test]
    fn unterminated_string_stops_at_the_length_limit() {
        let s = wrap(&format!("#1=A('{}", "x;".repeat(4000)));
        let opt = StreamOptions {
            chunk: 256,
            max_statement: 1024,
            ..Default::default()
        };
        match scan_with(s.as_bytes(), &opt) {
            Err(OpenError::Step(e)) => {
                assert!(matches!(e.kind, StepErrorKind::Expected(_)), "{e}")
            }
            other => panic!("{:?}", other.err()),
        }
    }

    #[test]
    fn every_prefix_fails_without_panic() {
        let s = wrap(BODY);
        let end = s.find("END-ISO-10303-21;").unwrap() + "END-ISO-10303-21;".len();
        for n in 0..end {
            for chunk in [1, 5, 4096] {
                assert!(
                    scan_mem(&s.as_bytes()[..n], chunk, false).is_err(),
                    "prefix {n}"
                );
            }
        }
        assert!(scan_mem(&s.as_bytes()[..end], 3, false).is_ok());
    }

    #[test]
    fn tolerant_matches_the_in_memory_tolerant_parse() {
        let s = wrap("#1=A(#9);\n#2=B(1);\n#2=C(#1);");
        let f = scan_mem(s.as_bytes(), 5, true).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f.type_of(2), Some("B"));
        let mem = StepFile::parse_tolerant(s.as_bytes()).unwrap();
        assert_eq!(f.warnings, mem.warnings);
        assert_eq!(f.warnings.len(), 2, "{:?}", f.warnings);
    }

    #[test]
    fn tolerant_skips_a_bad_header_record_but_never_the_schema() {
        let hdr = |schema: &str| {
            format!(
                "ISO-10303-21;\nHEADER;\nFILE_NAME('a',*/x*/(''));\n{schema}\nENDSEC;\nDATA;\n#1=A(1);\nENDSEC;\nEND-ISO-10303-21;\n"
            )
        };
        let s = hdr("FILE_SCHEMA(('IFC2X3'));");
        let f = scan_mem(s.as_bytes(), 4, true).unwrap();
        assert_eq!(f.schema, ["IFC2X3"]);
        assert_eq!(
            f.warnings,
            StepFile::parse_tolerant(s.as_bytes()).unwrap().warnings
        );
        assert!(scan_mem(s.as_bytes(), 4, false).is_err());
        let bad = hdr("FILE_SCHEMA(('IFC2X3'),*/x*/);");
        assert!(scan_mem(bad.as_bytes(), 4, true).is_err());
    }

    #[test]
    fn index_entry_is_compact() {
        assert!(std::mem::size_of::<Slot>() <= 16);
        let s = wrap(BODY);
        let f = scan_mem(s.as_bytes(), 4096, false).unwrap();
        assert!(f.ents.is_empty(), "streamed files keep slots only");
        assert_eq!(f.slots.len(), f.len());
    }

    fn temp(name: &str, body: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("ddc-g3-{}-{name}.ifc", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }

    fn points(n: u32) -> String {
        let mut data = String::from(BODY);
        for k in 0..n {
            data.push_str(&format!(
                "\n#{}=IFCCARTESIANPOINT(({k}.5,-{k}.25,0.));",
                1000 + k
            ));
        }
        wrap(&data)
    }

    #[test]
    fn file_open_decodes_lazily_through_small_pages() {
        let s = points(2000);
        let p = temp("lazy", &s);
        let opt = StreamOptions {
            chunk: 777,
            page: 4096,
            pages: 3,
            ..Default::default()
        };
        let f = StepFile::open_with(&p, &opt, None).unwrap();
        let full = StepFile::parse(s.as_bytes()).unwrap();
        same(&full, &f);
        assert!(f.cached_bytes() <= 3 * 4096);
        assert!(f.src.is_empty(), "no whole-file copy");
        assert_eq!(f.decode_failures(), 0);
        drop(f);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn leaf_reads_stay_within_the_cache_budget() {
        let s = points(5000);
        let opt = StreamOptions {
            leaves: 2048,
            ..opts(4096, false)
        };
        let f = scan_with(s.as_bytes(), &opt).unwrap();
        let mut sum = 0.0;
        for k in 0..5000u32 {
            let a = f.leaf(1000 + k).unwrap();
            sum += a[0].as_list().unwrap()[0].as_f64().unwrap();
        }
        assert!(sum > 0.0);
        let (pinned, leaves) = f.decoded_counts();
        assert_eq!(pinned, 0, "leaf reads pin nothing");
        // two generations of ~2 KB of ~35-byte spans
        assert!(leaves < 200, "{leaves} cached");
        // a pinned entity is served from its pin, and stays valid while more are pinned
        let e = f.get(1000).unwrap();
        for k in 1..500u32 {
            assert!(f.get(1000 + k).is_some());
        }
        assert_eq!(e.args()[0].as_list().unwrap()[0], Value::Real(0.5));
        assert_eq!(f.decoded_counts().0, 500);
        assert_eq!(&*f.leaf(1000).unwrap(), e.args());
    }

    #[test]
    fn changed_file_is_reported_not_silently_missing() {
        let s = points(10);
        let p = temp("changed", &s);
        let f = StepFile::open(&p, None).unwrap();
        std::fs::write(&p, "ISO-10303-21;").unwrap();
        assert!(f.get(1000).is_none());
        assert!(f.decode_failures() > 0);
        drop(f);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn cancel_stops_the_scan_and_the_decoding() {
        let s = wrap(BODY);
        let p = temp("cancel", &s);
        let c = Progress::new();
        c.cancel();
        let e = StepFile::open(&p, Some(c)).err().unwrap();
        assert_eq!(e, OpenError::Cancelled);
        let c = Progress::new();
        let f = StepFile::open(&p, Some(c.clone())).unwrap();
        assert!(f.get(1).is_some());
        c.cancel();
        assert!(f.get(2).is_none(), "undecoded entity after cancel");
        assert!(f.get(1).is_some(), "already decoded stays");
        assert_eq!(f.decode_failures(), 0, "cancel is not a failure");
        drop(f);
        let _ = std::fs::remove_file(&p);
    }

    /// 200 MB generated file streamed from disk: index time and resident index size.
    #[test]
    #[ignore]
    fn stream_budget_200mb() {
        let p = std::env::temp_dir().join(format!("ddc-g3-{}-big.ifc", std::process::id()));
        {
            use std::io::Write;
            let mut w = std::io::BufWriter::new(File::create(&p).unwrap());
            w.write_all(b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n")
                .unwrap();
            let (mut id, mut n) = (1u32, 0usize);
            while n < 200 << 20 {
                let k = id;
                let l = format!(
                    "#{k}=IFCCARTESIANPOINT(({}.5,-{}.25,0.));\n#{}=IFCPOLYLINE((#{k},#{k}));\n",
                    k % 1000,
                    k % 777,
                    k + 1
                );
                n += l.len();
                w.write_all(l.as_bytes()).unwrap();
                id += 2;
            }
            w.write_all(b"ENDSEC;\nEND-ISO-10303-21;\n").unwrap();
        }
        let t = web_time::Instant::now();
        let f = StepFile::open(&p, None).unwrap();
        let dt = t.elapsed().as_secs_f64();
        println!("stream index {} entities: {dt:.2} s", f.len());
        let t = web_time::Instant::now();
        let mut sum = 0.0;
        for (id, _) in f.iter().take(1_000_000) {
            if let Some(v) = f.leaf(id).and_then(|a| a.first().cloned()) {
                sum += v.as_list().map_or(0.0, |l| l.len() as f64);
            }
        }
        println!(
            "decode 1M: {:.2} s ({sum}), cached {:?}",
            t.elapsed().as_secs_f64(),
            f.decoded_counts()
        );
        drop(f);
        let _ = std::fs::remove_file(&p);
        assert!(dt <= 6.0, "{dt} s");
    }
}
