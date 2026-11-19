// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Byte-level view of a STEP file: statements with the bytes before them, kept exactly as read, so
//! a patched file differs from its source only where a statement was replaced, removed or added.

use std::io::{self, Read};

const CHUNK: usize = 1 << 20;

/// One statement and what precedes it (white space, comments). `body` ends with `;`; empty only
/// for bytes after the last statement.
pub(super) struct Piece<'a> {
    pub lead: &'a [u8],
    pub body: &'a [u8],
}

/// Streams a file statement by statement; a statement may span lines, or share one.
pub(super) struct Stmts<R> {
    r: R,
    chunk: Vec<u8>,
    at: usize,
    end: usize,
    cur: Vec<u8>,
}

impl<R: Read> Stmts<R> {
    pub fn new(r: R) -> Self {
        Self {
            r,
            chunk: vec![0; CHUNK],
            at: 0,
            end: 0,
            cur: Vec::new(),
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> io::Result<Option<Piece<'_>>> {
        self.cur.clear();
        let mut start: Option<usize> = None;
        let (mut in_str, mut comment) = (false, false);
        let mut prev = 0u8;
        loop {
            if self.at == self.end {
                self.end = self.r.read(&mut self.chunk)?;
                self.at = 0;
                if self.end == 0 {
                    if self.cur.is_empty() {
                        return Ok(None);
                    }
                    let n = start.unwrap_or(self.cur.len());
                    let (lead, body) = self.cur.split_at(n);
                    return Ok(Some(Piece { lead, body }));
                }
            }
            let base = self.cur.len();
            let mut done = None;
            for (i, &b) in self.chunk[self.at..self.end].iter().enumerate() {
                if comment {
                    if prev == b'*' && b == b'/' {
                        comment = false;
                        prev = 0;
                    } else {
                        prev = b;
                    }
                    continue;
                }
                if in_str {
                    in_str = b != b'\'';
                    continue;
                }
                if b == b'*' && prev == b'/' {
                    comment = true;
                    prev = 0;
                    continue;
                }
                if start.is_none() {
                    if prev == b'/' {
                        start = Some(base + i - 1);
                    } else if !matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'/') {
                        start = Some(base + i);
                    }
                }
                if b == b'\'' {
                    in_str = true;
                } else if b == b';' {
                    done = Some(i);
                    break;
                }
                prev = b;
            }
            match done {
                Some(i) => {
                    let stop = self.at + i + 1;
                    self.cur.extend_from_slice(&self.chunk[self.at..stop]);
                    self.at = stop;
                    let n = start.unwrap_or(base + i);
                    let (lead, body) = self.cur.split_at(n);
                    return Ok(Some(Piece { lead, body }));
                }
                None => {
                    self.cur.extend_from_slice(&self.chunk[self.at..self.end]);
                    self.at = self.end;
                }
            }
        }
    }
}

/// Parts of `#id=TYPE(args);`. A complex entity `#id=(A()B());` has an empty `ty`.
pub(super) struct Ent<'a> {
    pub id: u32,
    pub ty: &'a [u8],
    pub args: &'a [u8],
}

pub(super) fn trim(mut b: &[u8]) -> &[u8] {
    while let [x, rest @ ..] = b {
        if !x.is_ascii_whitespace() {
            break;
        }
        b = rest;
    }
    while let [rest @ .., x] = b {
        if !x.is_ascii_whitespace() {
            break;
        }
        b = rest;
    }
    b
}

/// The id of an entity statement, without parsing the rest.
pub(super) fn quick_id(body: &[u8]) -> Option<u32> {
    let rest = body.strip_prefix(b"#")?;
    let n = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    std::str::from_utf8(&rest[..n]).ok()?.parse().ok()
}

pub(super) fn parse_ent(body: &[u8]) -> Option<Ent<'_>> {
    let id = quick_id(trim(body))?;
    let eq = body.iter().position(|c| *c == b'=')?;
    let rhs = trim(&body[eq + 1..]);
    let rhs = trim(rhs.strip_suffix(b";").unwrap_or(rhs));
    let open = rhs.iter().position(|c| *c == b'(')?;
    let close = rhs.len().checked_sub(1).filter(|i| rhs[*i] == b')')?;
    if close < open {
        return None;
    }
    Some(Ent {
        id,
        ty: trim(&rhs[..open]),
        args: &rhs[open + 1..close],
    })
}

pub(super) fn upper(b: &[u8]) -> String {
    b.iter().map(|c| c.to_ascii_uppercase() as char).collect()
}

/// Top-level arguments, trimmed.
pub(super) fn split_top(args: &[u8]) -> Vec<&[u8]> {
    let mut out = vec![];
    let (mut depth, mut in_str, mut from) = (0i32, false, 0usize);
    for (i, &b) in args.iter().enumerate() {
        if in_str {
            in_str = b != b'\'';
            continue;
        }
        match b {
            b'\'' => in_str = true,
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                out.push(trim(&args[from..i]));
                from = i + 1;
            }
            _ => {}
        }
    }
    if from < args.len() || !out.is_empty() {
        out.push(trim(&args[from..]));
    }
    out
}

/// Items between an argument's outer parentheses.
pub(super) fn list_items(arg: &[u8]) -> Option<Vec<&[u8]>> {
    let inner = arg.strip_prefix(b"(")?.strip_suffix(b")")?;
    Some(split_top(inner))
}

pub(super) fn single_ref(arg: &[u8]) -> Option<u32> {
    let rest = arg.strip_prefix(b"#")?;
    if rest.is_empty() || !rest.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(rest).ok()?.parse().ok()
}

pub(super) fn join_args(a: &[Vec<u8>]) -> Vec<u8> {
    a.join(&b","[..])
}

pub(super) fn owned(a: &[&[u8]]) -> Vec<Vec<u8>> {
    a.iter().map(|x| x.to_vec()).collect()
}

pub(super) fn ref_list(ids: &[u32]) -> Vec<u8> {
    let t: Vec<String> = ids.iter().map(|i| format!("#{i}")).collect();
    format!("({})", t.join(",")).into_bytes()
}

/// Every `#n` outside strings.
pub(super) fn for_each_ref(args: &[u8], f: &mut dyn FnMut(u32)) {
    let (mut i, mut in_str) = (0, false);
    while i < args.len() {
        let b = args[i];
        i += 1;
        if in_str {
            in_str = b != b'\'';
        } else if b == b'\'' {
            in_str = true;
        } else if b == b'#' {
            let n = args[i..].iter().take_while(|c| c.is_ascii_digit()).count();
            if let Some(v) = std::str::from_utf8(&args[i..i + n])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                f(v);
            }
            i += n;
        }
    }
}

/// `args` with every `#n` replaced by `f(n)`, everything else byte for byte.
pub(super) fn map_refs<E>(
    args: &[u8],
    f: &mut dyn FnMut(u32) -> Result<u32, E>,
) -> Result<Vec<u8>, E> {
    let mut out = Vec::with_capacity(args.len());
    let (mut i, mut in_str) = (0, false);
    while i < args.len() {
        let b = args[i];
        i += 1;
        if in_str {
            in_str = b != b'\'';
            out.push(b);
        } else if b == b'\'' {
            in_str = true;
            out.push(b);
        } else if b == b'#' {
            let n = args[i..].iter().take_while(|c| c.is_ascii_digit()).count();
            match std::str::from_utf8(&args[i..i + n])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(v) => out.extend_from_slice(format!("#{}", f(v)?).as_bytes()),
                None => {
                    out.push(b'#');
                    out.extend_from_slice(&args[i..i + n]);
                }
            }
            i += n;
        } else {
            out.push(b);
        }
    }
    Ok(out)
}

/// `arg` with every real number multiplied by `k`; strings, references, enums and type names kept.
pub(super) fn scale_numbers(arg: &[u8], k: f64) -> Vec<u8> {
    let mut out = Vec::with_capacity(arg.len() + 8);
    let mut i = 0;
    while i < arg.len() {
        let b = arg[i];
        match b {
            b'\'' => {
                let n = arg[i + 1..]
                    .iter()
                    .position(|c| *c == b'\'')
                    .map_or(arg.len() - i - 1, |p| p + 1);
                out.extend_from_slice(&arg[i..=(i + n).min(arg.len() - 1)]);
                i += n + 1;
            }
            b'#' => {
                let n = arg[i + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_digit())
                    .count();
                out.extend_from_slice(&arg[i..=i + n]);
                i += n + 1;
            }
            b'.' if arg.get(i + 1).is_some_and(u8::is_ascii_alphabetic) => {
                let n = arg[i + 1..]
                    .iter()
                    .position(|c| *c == b'.')
                    .map_or(arg.len() - i - 1, |p| p + 1);
                out.extend_from_slice(&arg[i..=(i + n).min(arg.len() - 1)]);
                i += n + 1;
            }
            c if c.is_ascii_alphabetic() => {
                let n = arg[i..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_')
                    .count();
                out.extend_from_slice(&arg[i..i + n]);
                i += n;
            }
            c if c.is_ascii_digit() || c == b'-' || c == b'+' || c == b'.' => {
                let n = arg[i..]
                    .iter()
                    .take_while(|c| {
                        c.is_ascii_digit() || matches!(**c, b'.' | b'-' | b'+' | b'e' | b'E')
                    })
                    .count();
                let v = std::str::from_utf8(&arg[i..i + n])
                    .ok()
                    .and_then(|s| s.parse::<f64>().ok());
                match v {
                    Some(v) => out.extend_from_slice(crate::ifc::step::real(v * k).as_bytes()),
                    None => out.extend_from_slice(&arg[i..i + n]),
                }
                i += n;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// The first argument, when it is a GlobalId.
pub(super) fn guid_of(args: &[u8]) -> Option<&str> {
    let a = args.iter().position(|c| !c.is_ascii_whitespace())?;
    let g = args.get(a + 1..a + 23)?;
    if args[a] != b'\'' || args.get(a + 23) != Some(&b'\'') {
        return None;
    }
    let s = std::str::from_utf8(g).ok()?;
    crate::ifc::is_global_id(s).then_some(s)
}

/// A string argument as text: quotes off, `''` undone; encoded text goes through the reader.
pub(super) fn string_arg(raw: &[u8]) -> Option<String> {
    let inner = raw.strip_prefix(b"'")?.strip_suffix(b"'")?;
    if inner.contains(&b'\\') || !inner.is_ascii() {
        let mut s = b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=X(".to_vec();
        s.extend_from_slice(raw);
        s.extend_from_slice(b");\nENDSEC;\nEND-ISO-10303-21;\n");
        let f = crate::ifc::StepFile::parse_tolerant(&s).ok()?;
        return f.get(1)?.args().first()?.as_str().map(str::to_string);
    }
    Some(String::from_utf8_lossy(inner).replace("''", "'"))
}

/// Growable set of ids.
#[derive(Default)]
pub(super) struct Bits(Vec<u64>);

impl Bits {
    /// True when newly set.
    pub fn set(&mut self, id: u32) -> bool {
        let w = (id / 64) as usize;
        if w >= self.0.len() {
            self.0.resize(w + 1, 0);
        }
        let m = 1u64 << (id % 64);
        let new = self.0[w] & m == 0;
        self.0[w] |= m;
        new
    }

    /// Ids set here and not in `other`.
    pub fn count_without(&self, other: &Bits) -> u64 {
        self.0
            .iter()
            .enumerate()
            .map(|(i, w)| (w & !other.0.get(i).copied().unwrap_or(0)).count_ones() as u64)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(s: &str) -> Vec<(String, String)> {
        let mut st = Stmts::new(s.as_bytes());
        let mut v = vec![];
        while let Some(p) = st.next().unwrap() {
            v.push((
                String::from_utf8_lossy(p.lead).to_string(),
                String::from_utf8_lossy(p.body).to_string(),
            ));
        }
        v
    }

    #[test]
    fn pieces_join_back_to_the_same_bytes() {
        let s = "A;\r\n/* c; */\n#1=B('x;''y;',\n#2);#2=C();\n\n tail";
        let v = pieces(s);
        let j: String = v.iter().map(|(a, b)| format!("{a}{b}")).collect();
        assert_eq!(j, s);
        assert_eq!(v[1].0, "\r\n/* c; */\n");
        assert_eq!(v[1].1, "#1=B('x;''y;',\n#2);");
        assert_eq!(v[2].1, "#2=C();");
        assert_eq!(v[3].1, "tail");
    }

    #[test]
    fn a_statement_longer_than_a_chunk_stays_whole() {
        let long = "x".repeat(CHUNK * 2 + 17);
        let s = format!("#1=A('{long}');\n#2=B();");
        let v = pieces(&s);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].1.len(), long.len() + 9);
    }

    #[test]
    fn arguments_split_at_the_top_level_only() {
        let a = split_top(b"'a,b',(#1,#2), $ ,IFCLABEL('x,y')");
        assert_eq!(a.len(), 4);
        assert_eq!(a[2], b"$");
        assert_eq!(list_items(a[1]).unwrap(), vec![&b"#1"[..], &b"#2"[..]]);
    }

    #[test]
    fn references_in_strings_are_not_references() {
        let m: Result<_, ()> = map_refs(b"'#1',#1,(#22)", &mut |r| Ok(r + 100));
        assert_eq!(m.unwrap(), b"'#1',#101,(#122)".to_vec());
    }

    #[test]
    fn only_numbers_scale() {
        let v = scale_numbers(b"(1.,2.5,-3.),.T.,#7,'4.',IFCLENGTHMEASURE(2.)", 1000.0);
        assert_eq!(
            String::from_utf8(v).unwrap(),
            "(1000.,2500.,-3000.),.T.,#7,'4.',IFCLENGTHMEASURE(2000.)"
        );
    }

    #[test]
    fn an_entity_is_read_with_spaces() {
        let e = parse_ent(b"#12 = IFCWALL ('a',$) ;").unwrap();
        assert_eq!((e.id, e.ty, e.args), (12, &b"IFCWALL"[..], &b"'a',$"[..]));
    }

    #[test]
    fn a_guid_is_exactly_22_characters() {
        assert_eq!(
            guid_of(b"'0000000000000000000001',$"),
            Some("0000000000000000000001")
        );
        assert_eq!(guid_of(b"'short',$"), None);
    }
}
