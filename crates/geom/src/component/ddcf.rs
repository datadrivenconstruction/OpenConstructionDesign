//! `.ddcf` text format, version 2: header (`ddcf 2`, `version`, `uid`), canonical printer,
//! pure v1 -> v2 upgrade and errors with `line:col`. Text without a `ddcf` line is v1 and reads
//! exactly as before.
//!
//! ```text
//! ddcf 2                 # format; must be the first statement
//! family Railing
//! version 1.4.0          # family version: major.minor.patch
//! uid 7f3c0a12d4e5b6a7   # stable id (8..64 hex digits), survives renames
//! ```
//!
//! Canonical form: one statement per line, single spaces outside quotes, no blank lines, header
//! first, then sections in a fixed order (params, limits, refs, sketch, types, solids, symbols,
//! connectors, visibility and nests, nested blocks). Order inside a section is kept, so the
//! parsed family is unchanged; comments travel with the statement below them.

use super::{doc, Family, FamilyError};
use std::fmt;

/// Newest format this build reads and writes.
pub const FORMAT: u32 = 2;

/// Family version `major.minor.patch`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let w: Vec<&str> = s.split('.').collect();
        let num = |t: &str| -> Option<u32> {
            if t.is_empty() || t.len() > 9 || !t.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            t.parse().ok()
        };
        match *w.as_slice() {
            [a, b, c] => Some(Version {
                major: num(a)?,
                minor: num(b)?,
                patch: num(c)?,
            }),
            _ => None,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Header of a family text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    /// 1 when there is no `ddcf` line.
    pub format: u32,
    pub version: Option<Version>,
    pub uid: Option<String>,
}

impl Default for Meta {
    fn default() -> Self {
        Meta {
            format: 1,
            version: None,
            uid: None,
        }
    }
}

pub fn uid_ok(s: &str) -> bool {
    (8..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Header statement `head rest`; `index` is the statement's position in the text (0 = first).
pub(super) fn header(m: &mut Meta, index: usize, head: &str, rest: &str) -> Result<(), String> {
    match head {
        "ddcf" => {
            if index != 0 {
                return Err("'ddcf' must be the first statement".into());
            }
            let n: u32 = rest
                .parse()
                .map_err(|_| format!("bad format '{rest}', expected 'ddcf {FORMAT}'"))?;
            if n == 0 || n > FORMAT {
                return Err(format!(
                    "unsupported format '{rest}', this build reads ddcf 1..{FORMAT}"
                ));
            }
            m.format = n;
        }
        "version" | "uid" => {
            if m.format < 2 {
                return Err(format!("'{head}' needs the 'ddcf 2' header on the first line"));
            }
            if head == "version" {
                if m.version.is_some() {
                    return Err("duplicate 'version'".into());
                }
                m.version = Some(Version::parse(rest).ok_or_else(|| {
                    format!("bad version '{rest}', expected major.minor.patch like 1.0.0")
                })?);
            } else {
                if m.uid.is_some() {
                    return Err("duplicate 'uid'".into());
                }
                if !uid_ok(rest) {
                    return Err(format!("bad uid '{rest}', expected 8 to 64 hex digits 0-9a-f"));
                }
                m.uid = Some(rest.to_string());
            }
        }
        o => return Err(format!("unknown statement '{o}'")),
    }
    Ok(())
}

/// Validation error with a 1-based position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DdcfError {
    pub line: usize,
    pub col: usize,
    pub msg: String,
}

impl fmt::Display for DdcfError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

impl std::error::Error for DdcfError {}

/// Parse and validate; errors point at a line and column.
pub fn check(text: &str) -> Result<Family, DdcfError> {
    Family::parse(text).map_err(|e| locate(text, &e))
}

fn col_of(line: &str, tok: &str) -> Option<usize> {
    if tok.is_empty() {
        return None;
    }
    line.find(tok).map(|k| line[..k].chars().count() + 1)
}

fn first_col(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count() + 1
}

/// Position of a `FamilyError` in `text`: the statement it names, the quoted token of the
/// message inside that statement, else the statement's first character.
pub fn locate(text: &str, e: &FamilyError) -> DdcfError {
    let lines: Vec<&str> = text.lines().collect();
    let at = |line: usize, tok: Option<&str>, msg: String| {
        let line = line.clamp(1, lines.len().max(1));
        let span = doc::stmts(text)
            .into_iter()
            .find(|s| s.first < line && line <= s.last + 1)
            .map_or(line..=line, |s| line..=s.last + 1);
        for n in span {
            let l = lines.get(n - 1).copied().unwrap_or("");
            if let Some(c) = tok.and_then(|t| col_of(l, t)) {
                return DdcfError { line: n, col: c, msg };
            }
        }
        let col = first_col(lines.get(line - 1).copied().unwrap_or(""));
        DdcfError { line, col, msg }
    };
    let named = |name: &str, msg: String| {
        let s = doc::stmts(text);
        let hit = s
            .iter()
            .find(|s| s.head == "param" && s.name == name)
            .or_else(|| s.iter().find(|s| s.name == name));
        match hit {
            Some(s) => at(s.first + 1, Some(name), msg),
            None => at(1, None, msg),
        }
    };
    match e {
        FamilyError::Parse { line, msg } => {
            if let Some(inner) = msg.strip_prefix("nested family: line ") {
                if let Some((n, rest)) = inner.split_once(": ") {
                    if let Ok(n) = n.parse::<usize>() {
                        let sub = FamilyError::Parse {
                            line: line + n,
                            msg: rest.to_string(),
                        };
                        return locate(text, &sub);
                    }
                }
            }
            let tok = msg.split('\'').nth(1).or_else(|| {
                msg.strip_prefix("duplicate ")
                    .and_then(|r| r.split_whitespace().last())
            });
            at(*line, tok, msg.clone())
        }
        FamilyError::Cycle(p) => named(p.first().map_or("", |s| s.as_str()), e.to_string()),
        FamilyError::Param { name, .. } => named(name, e.to_string()),
        FamilyError::Geometry { solid, .. } => named(solid, e.to_string()),
    }
}

/// Code part and comment of a line, split exactly like the parser does.
fn split_comment(line: &str) -> (&str, Option<String>) {
    match line.find('#') {
        Some(k) if !line[..k].contains('"') || line[..k].matches('"').count() % 2 == 0 => {
            (&line[..k], Some(line[k..].trim().to_string()))
        }
        _ => (line, None),
    }
}

fn parens(code: &str) -> i32 {
    code.matches('(').count() as i32 - code.matches(')').count() as i32
}

/// Single spaces outside double quotes; the name of `family` stays as written.
fn normalize(code: &str) -> String {
    let code = code.trim();
    let (head, rest) = code.split_once(char::is_whitespace).unwrap_or((code, ""));
    if head == "family" {
        return format!("family {}", rest.trim());
    }
    let mut out = String::with_capacity(code.len());
    let (mut q, mut sp) = (false, false);
    for ch in code.chars() {
        if ch == '"' {
            q = !q;
        }
        if !q && ch.is_whitespace() {
            sp = true;
            continue;
        }
        if sp {
            out.push(' ');
            sp = false;
        }
        out.push(ch);
    }
    out
}

/// Section of a statement in canonical order.
pub(super) fn rank(head: &str) -> u8 {
    match head {
        "ddcf" => 0,
        "family" => 1,
        "version" => 2,
        "uid" => 3,
        "status" => 4,
        "category" => 5,
        "host" => 6,
        "ifc" => 7,
        "param" => 10,
        "limit" => 11,
        "ref" => 12,
        "refline" | "dim" | "eq" | "align" | "angle" => 13,
        "type" => 14,
        "extrude" | "revolve" | "sweep" | "blend" | "mesh" | "feature" | "plant" => 15,
        "symbol" => 16,
        "connector" => 17,
        "visible" | "origin" | "nest" => 18,
        "begin" => 30,
        _ => UNRANKED,
    }
}

/// Rank of a head the printer does not know; `ddcf_every_operator_has_a_rank` keeps every
/// parser statement off it.
pub(super) const UNRANKED: u8 = 20;

struct Item {
    rank: u8,
    pre: Vec<String>,
    code: String,
    tail: Vec<String>,
    /// Nested `begin … end`: printed body and the `end` line.
    block: Option<(String, String)>,
}

fn head_of(code: &str) -> &str {
    code.split_whitespace().next().unwrap_or("")
}

fn with_tail(code: &str, tail: &[String]) -> String {
    if tail.is_empty() {
        code.to_string()
    } else if code.is_empty() {
        tail.join(" ")
    } else {
        format!("{code} {}", tail.join(" "))
    }
}

/// Canonical text; `header` (if any) replaces the `ddcf` line, and every line sorts by `rank`:
/// `ddcf` before `family`, `version` and `uid` right after it.
fn print(text: &str, header: Option<&[String]>) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let (mut preamble, mut items, mut pending) = (vec![], Vec::<Item>::new(), vec![]);
    let (mut open, mut i) = (0i32, 0usize);
    while i < lines.len() {
        let (code, comment) = split_comment(lines[i]);
        if open > 0 {
            let it = items.last_mut().expect("open statement");
            if !code.trim().is_empty() {
                it.code.push(' ');
                it.code.push_str(code.trim());
            }
            it.tail.extend(comment);
            open += parens(code);
            i += 1;
            continue;
        }
        if code.trim().is_empty() {
            pending.extend(comment);
            i += 1;
            continue;
        }
        if items.is_empty() {
            preamble.append(&mut pending);
        }
        let mut it = Item {
            rank: rank(head_of(code)),
            pre: std::mem::take(&mut pending),
            code: code.trim().to_string(),
            tail: comment.into_iter().collect(),
            block: None,
        };
        if head_of(code) == "begin" {
            let (mut depth, mut inner, mut end) = (1usize, vec![], String::from("end"));
            i += 1;
            while i < lines.len() {
                let (c, cm) = split_comment(lines[i]);
                match head_of(c) {
                    "begin" => depth += 1,
                    "end" => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    end = with_tail("end", &cm.into_iter().collect::<Vec<_>>());
                    break;
                }
                inner.push(lines[i]);
                i += 1;
            }
            let body = print(&inner.join("\n"), None);
            it.code = normalize(&it.code);
            it.block = Some((body, end));
            items.push(it);
            i += 1;
            continue;
        }
        open = parens(code);
        items.push(it);
        i += 1;
    }
    for it in items.iter_mut() {
        if it.block.is_none() {
            it.code = normalize(&it.code);
        }
    }
    if let Some(h) = header {
        items.retain(|it| head_of(&it.code) != "ddcf");
        for line in h {
            items.push(Item {
                rank: rank(head_of(line)),
                pre: vec![],
                code: line.clone(),
                tail: vec![],
                block: None,
            });
        }
    }
    items.sort_by_key(|it| it.rank);
    let mut out: Vec<String> = preamble;
    for it in items {
        out.extend(it.pre);
        out.push(with_tail(&it.code, &it.tail));
        if let Some((body, end)) = it.block {
            out.extend(body.lines().map(str::to_string));
            out.push(end);
        }
    }
    out.extend(pending);
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Canonical text of a valid family, keeping its format version.
pub fn fmt(text: &str) -> Result<String, DdcfError> {
    check(text)?;
    Ok(print(text, None))
}

/// Stable id of a canonical text: two 64-bit FNV-1a hashes, 32 hex digits.
pub fn uid_for(canonical: &str) -> String {
    let fnv = |basis: u64| {
        canonical.bytes().fold(basis, |h, b| {
            (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
        })
    };
    let a = fnv(0xcbf2_9ce4_8422_2325);
    let b = fnv(0xcbf2_9ce4_8422_2325 ^ 0x9e37_79b9_7f4a_7c15);
    format!("{a:016x}{b:016x}")
}

/// The text with its own `uid` statement (not a nested family's) set to `uid`; `None` when the
/// family has no `uid` line. A trailing comment on that line is kept.
pub fn replace_uid(text: &str, uid: &str) -> Option<String> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    let mut done = false;
    for line in text.lines() {
        let (code, comment) = split_comment(line);
        match head_of(code) {
            "begin" => depth += 1,
            "end" => depth = depth.saturating_sub(1),
            "uid" if depth == 0 && !done => {
                done = true;
                out.push(with_tail(&format!("uid {uid}"), &comment.into_iter().collect::<Vec<_>>()));
                continue;
            }
            _ => {}
        }
        out.push(line.to_string());
    }
    done.then(|| out.join("\n") + "\n")
}

/// Pure upgrade v1 -> v2: header `ddcf 2`, `version 1.0.0`, `uid` from the canonical v1 text.
/// A v2 text comes back canonical and otherwise unchanged.
pub fn upgrade(text: &str) -> Result<String, DdcfError> {
    let f = check(text)?;
    if f.meta.format >= FORMAT {
        return Ok(print(text, None));
    }
    let uid = uid_for(&print(text, None));
    let h = [
        format!("ddcf {FORMAT}"),
        "version 1.0.0".to_string(),
        format!("uid {uid}"),
    ];
    Ok(print(text, Some(&h)))
}
