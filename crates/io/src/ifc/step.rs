// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! STEP physical file writer: sequential ids (deterministic), value formatting, GlobalIds, self-check.

use std::fmt::Write as _;
use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::path::Path;
use std::sync::Arc;

use super::export::sink::Sink;
use super::schema_spec::IfcSchema;
use crate::job::{Progress, CANCELLED};

/// End of every file, after the DATA section.
pub(crate) const TRAILER: &str = "ENDSEC;\nEND-ISO-10303-21;\n";

/// Everything before the first entity line.
pub(crate) fn header(file_name: &str, schema: IfcSchema, stamp: &str) -> String {
    let view = if schema == IfcSchema::Ifc2x3 {
        "CoordinationView_V2.0"
    } else {
        "ReferenceView_V1.2"
    };
    let name = text(file_name);
    let schema = schema.header_name();
    format!(
        "ISO-10303-21;\nHEADER;\n\
         FILE_DESCRIPTION(('ViewDefinition [{view}]'),'2;1');\n\
         FILE_NAME({name},'{stamp}',(''),(''),'ddc-io','ddc-io','');\n\
         FILE_SCHEMA(('{schema}'));\nENDSEC;\nDATA;\n"
    )
}

/// FILE_NAME time of a body the post-pass has yet to finish.
pub(crate) const EPOCH_STAMP: &str = "1970-01-01T00:00:00";

/// Entity lines kept in memory, or passed straight to a [`Sink`] when built with [`Step::streaming`].
#[derive(Default)]
pub struct Step {
    lines: Vec<String>,
    next: u32,
    sink: Option<Sink>,
}

impl Step {
    pub(crate) fn streaming(sink: Sink) -> Self {
        Self {
            lines: Vec::new(),
            next: 0,
            sink: Some(sink),
        }
    }

    /// Appends `#n=NAME(args);` and returns n.
    pub fn add(&mut self, name: &str, args: &[String]) -> u32 {
        self.next += 1;
        let n = self.next;
        let mut line = String::with_capacity(
            name.len() + 16 + args.iter().map(|a| a.len() + 1).sum::<usize>(),
        );
        let _ = write!(line, "#{n}={name}(");
        for (k, a) in args.iter().enumerate() {
            if k > 0 {
                line.push(',');
            }
            line.push_str(a);
        }
        line.push_str(");");
        match &mut self.sink {
            Some(s) => s.line(n, args.first().map(String::as_str), &line),
            None => self.lines.push(line),
        }
        n
    }

    /// Entities whose first argument is one of `guids` (as written by [`guid`]): one pass.
    pub fn find_rooted(&self, guids: &HashSet<String>) -> HashMap<String, u32> {
        if let Some(s) = &self.sink {
            return s.find_rooted(guids);
        }
        let mut out = HashMap::new();
        for (i, l) in self.lines.iter().enumerate() {
            let Some(a) = l.find('(') else { continue };
            let Some(first) = l[a + 1..].split(',').next() else {
                continue;
            };
            if guids.contains(first) {
                out.entry(first.to_string()).or_insert(i as u32 + 1);
            }
        }
        out
    }

    /// Why a streaming export cannot go on (full disk, cancel): the caller stops early.
    pub(crate) fn alive(&self) -> Result<(), String> {
        match self.sink.as_ref().and_then(Sink::stopped) {
            Some(e) => Err(e.to_string()),
            None => Ok(()),
        }
    }

    /// The sink of a streaming writer, to finish the file.
    pub(crate) fn into_sink(self) -> Option<Sink> {
        self.sink
    }

    pub fn finish(self, file_name: &str) -> String {
        let mut out = header(file_name, IfcSchema::Ifc4x3, EPOCH_STAMP);
        for l in &self.lines {
            out.push_str(l);
            out.push('\n');
        }
        out.push_str(TRAILER);
        out
    }
}

/// Checked variant of [`real`]: non-finite values are an error, not `inf`/`NaN` in the file.
pub fn real_checked(v: f64) -> Result<String, String> {
    if v.is_finite() {
        Ok(real(v))
    } else {
        Err(format!("non-finite real {v}"))
    }
}

/// Unchecked formatter; non-finite output is caught by [`check_step`].
pub fn real(v: f64) -> String {
    let v = if v == 0.0 { 0.0 } else { v };
    let mut s = String::with_capacity(24);
    let _ = write!(s, "{v}");
    if v.is_finite() && !s.contains(['.', 'e']) {
        s.push('.');
    }
    s
}

pub fn text(s: &str) -> String {
    let mut o = String::from("'");
    for c in s.chars() {
        match c {
            '\'' => o.push_str("''"),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x80 && !c.is_control() => o.push(c),
            c => {
                let mut b = [0u16; 2];
                o.push_str("\\X2\\");
                for u in c.encode_utf16(&mut b) {
                    o.push_str(&format!("{u:04X}"));
                }
                o.push_str("\\X0\\");
            }
        }
    }
    o.push('\'');
    o
}

pub fn r(id: u32) -> String {
    format!("#{id}")
}

pub fn list(ids: &[u32]) -> String {
    let mut o = String::with_capacity(2 + ids.len() * 8);
    o.push('(');
    for (k, i) in ids.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        let _ = write!(o, "#{i}");
    }
    o.push(')');
    o
}

pub fn reals(vs: &[f64]) -> String {
    let mut o = String::with_capacity(2 + vs.len() * 12);
    o.push('(');
    for (k, x) in vs.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        o.push_str(&real(*x));
    }
    o.push(')');
    o
}

fn fnv64(seed: u64, bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64 ^ seed;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

const GUID_CH: &[u8; 64] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_$";

/// 128 bits -> 22-character IFC GlobalId text (no quotes).
pub fn compress_guid(mut v: u128) -> String {
    let mut out = [0u8; 22];
    for i in (1..22).rev() {
        out[i] = GUID_CH[(v & 63) as usize];
        v >>= 6;
    }
    out[0] = GUID_CH[(v & 3) as usize];
    String::from_utf8(out.to_vec()).unwrap_or_default()
}

/// Stable 128-bit hash of a key.
pub fn hash128(key: &str) -> u128 {
    let hi = fnv64(0, key.as_bytes());
    let lo = fnv64(0x9e3779b97f4a7c15, key.as_bytes());
    ((hi as u128) << 64) | lo as u128
}

/// A well-formed IFC GlobalId: 22 characters of the IFC alphabet, first one 0..3.
pub fn is_global_id(s: &str) -> bool {
    s.len() == 22
        && s.bytes().all(|b| GUID_CH.contains(&b))
        && matches!(s.as_bytes()[0], b'0'..=b'3')
}

/// 22-character IFC GlobalId (quoted STEP string) from a hash of (kind, id).
pub fn guid(kind: &str, id: i64) -> String {
    format!("'{}'", compress_guid(hash128(&format!("{kind}:{id}"))))
}

/// Line-by-line form of [`check_step`], so a file of any size is checked without holding it.
#[derive(Default)]
pub struct StepCheck {
    ids: HashSet<u32>,
    refs: Vec<(u32, u32)>,
}

impl StepCheck {
    /// One line of the DATA section.
    pub fn line(&mut self, line: &str) -> Result<(), String> {
        if line.is_empty() {
            return Ok(());
        }
        if !line.ends_with(");") {
            return Err(format!("line does not end with ');': {line}"));
        }
        let (head, body) = line.split_once('=').ok_or(format!("no '=': {line}"))?;
        let id: u32 = head
            .strip_prefix('#')
            .and_then(|s| s.parse().ok())
            .ok_or(format!("bad id: {line}"))?;
        if !self.ids.insert(id) {
            return Err(format!("duplicate id #{id}"));
        }
        let (mut in_str, mut depth) = (false, 0i32);
        if body.split('\'').step_by(2).any(|seg| {
            seg.split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
                .any(|t| matches!(t, "inf" | "-inf" | "NaN" | "nan"))
        }) {
            return Err(format!("non-finite real: {line}"));
        }
        let b = body.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\'' => in_str = !in_str,
                b'(' if !in_str => depth += 1,
                b')' if !in_str => depth -= 1,
                b'#' if !in_str => {
                    let j = body[i + 1..]
                        .find(|c: char| !c.is_ascii_digit())
                        .map_or(body.len(), |k| i + 1 + k);
                    let n: u32 = body[i + 1..j]
                        .parse()
                        .map_err(|_| format!("bad ref: {line}"))?;
                    self.refs.push((id, n));
                    i = j - 1;
                }
                _ => {}
            }
            i += 1;
        }
        if in_str || depth != 0 {
            return Err(format!("unbalanced: {line}"));
        }
        Ok(())
    }

    /// Every reference resolves. Returns the entity count.
    pub fn finish(self) -> Result<usize, String> {
        for (from, to) in self.refs {
            if !self.ids.contains(&to) {
                return Err(format!("#{from} references missing #{to}"));
            }
        }
        Ok(self.ids.len())
    }
}

/// Checks the DATA section: every entity line is `#n=NAME(...);`, ids are unique,
/// every `#ref` outside strings resolves. Returns the entity count.
pub fn check_step(text: &str) -> Result<usize, String> {
    let data = text
        .split("DATA;\n")
        .nth(1)
        .and_then(|t| t.split("ENDSEC;").next())
        .ok_or("no DATA section")?;
    let mut c = StepCheck::default();
    for line in data.lines() {
        c.line(line)?;
    }
    c.finish()
}

/// [`check_step`] of a file on disk, reading it as written. A file cut short has no end marker
/// and fails here. `ctl` counts bytes and stops the check on cancel.
pub fn check_step_file(path: &Path, ctl: Option<&Arc<Progress>>) -> Result<usize, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if let Some(c) = ctl {
        c.set_total(f.metadata().map_err(|e| e.to_string())?.len());
    }
    let mut rd = std::io::BufReader::with_capacity(1 << 20, f);
    let mut buf = String::new();
    let mut check = StepCheck::default();
    #[derive(PartialEq)]
    enum At {
        Head,
        Data,
        Tail,
        End,
    }
    let mut at = At::Head;
    loop {
        buf.clear();
        let n = rd.read_line(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if let Some(c) = ctl {
            c.add(n as u64);
            if c.is_cancelled() {
                return Err(CANCELLED.into());
            }
        }
        let line = buf.trim_end_matches(['\r', '\n']);
        match at {
            At::Head if line == "DATA;" => at = At::Data,
            At::Head => {}
            At::Data if line == "ENDSEC;" => at = At::Tail,
            At::Data => check.line(line)?,
            At::Tail if line == "END-ISO-10303-21;" => at = At::End,
            At::Tail | At::End => {}
        }
    }
    if at != At::End {
        return Err("file is cut short: no end marker".into());
    }
    check.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_real_is_error() {
        assert!(real_checked(f64::NAN).is_err());
        assert!(real_checked(f64::NEG_INFINITY).is_err());
        assert_eq!(real_checked(2.0).unwrap(), "2.");
        let mut s = Step::default();
        s.add("IFCCARTESIANPOINT", &[reals(&[1.0, f64::INFINITY])]);
        assert!(check_step(&s.finish("a")).is_err());
        let mut s = Step::default();
        s.add("IFCLABEL", &[text("inf NaN")]);
        assert!(check_step(&s.finish("a")).is_ok());
    }

    fn file(tag: &str, body: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("ddc-o5-step-{tag}-{}.ifc", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn the_file_check_agrees_with_the_text_check() {
        let mut s = Step::default();
        let a = s.add("IFCDIRECTION", &[reals(&[0.0, 0.0, 1.0])]);
        s.add("IFCLABEL", &[r(a)]);
        let text = s.finish("a");
        let p = file("ok", &text);
        assert_eq!(check_step_file(&p, None), check_step(&text));
        assert_eq!(check_step_file(&p, None), Ok(2));
    }

    #[test]
    fn a_cut_file_fails_the_file_check() {
        let mut s = Step::default();
        s.add("IFCDIRECTION", &[reals(&[0.0, 0.0, 1.0])]);
        s.add("IFCDIRECTION", &[reals(&[1.0, 0.0, 0.0])]);
        let text = s.finish("a");
        let cut = &text[..text.find("#2=").unwrap()];
        assert!(check_step_file(&file("cut", cut), None).is_err());
        let no_end = text.replace("END-ISO-10303-21;\n", "");
        assert!(check_step_file(&file("noend", &no_end), None).is_err());
        let half_line = &text[..text.find("#2=").unwrap() + 6];
        assert!(check_step_file(&file("half", half_line), None).is_err());
    }

    #[test]
    fn the_file_check_finds_a_dangling_reference() {
        let mut s = Step::default();
        s.add("IFCLABEL", &["#99".to_string()]);
        assert!(check_step_file(&file("ref", &s.finish("a")), None).is_err());
    }
}
