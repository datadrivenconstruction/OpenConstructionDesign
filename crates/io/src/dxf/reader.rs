//! DXF ASCII reader. Accepts LF and CRLF.

use super::entities::{is_structural, parse_all, Entity};
use super::*;
use std::path::Path;

#[cfg(any(test, feature = "dxf-write"))]
fn tokenize(text: &str) -> Result<Vec<Pair>, DxfError> {
    tokenize_with(text, &|| false)
}

/// Binary DXF sentinel: 22 bytes, a 7-letter producer tag and a space, then
/// "Binary DXF\r\n", 0x1A, 0x00.
pub(super) fn is_binary_dxf(b: &[u8]) -> bool {
    const TAIL: &[u8] = b"Binary DXF\r\n\x1a\0";
    b.len() >= 8 + TAIL.len()
        && b[..7].iter().all(u8::is_ascii_alphabetic)
        && b[7] == b' '
        && &b[8..8 + TAIL.len()] == TAIL
}

fn cancelled() -> DxfError {
    DxfError(crate::job::CANCELLED.into())
}

/// [`tokenize`] that polls `stop` every 64k lines.
fn tokenize_with(text: &str, stop: &dyn Fn() -> bool) -> Result<Vec<Pair>, DxfError> {
    if is_binary_dxf(text.as_bytes()) {
        return Err(DxfError("binary DXF is not supported".into()));
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let mut out = vec![];
    let mut n = 0usize;
    while let Some(c) = lines.next() {
        n += 1;
        if n % 65_536 == 0 && stop() {
            return Err(cancelled());
        }
        if c.trim().is_empty() && lines.clone().all(|l| l.trim().is_empty()) {
            break; // trailing blank lines
        }
        let code = c
            .trim()
            .parse::<i32>()
            .map_err(|_| DxfError(format!("line {n}: bad group code {c:?}")))?;
        let v = lines
            .next()
            .ok_or_else(|| DxfError(format!("line {n}: missing value for code {code}")))?;
        n += 1;
        if code == 999 {
            continue; // comment
        }
        let float_code = matches!(code, 10..=59 | 110..=149 | 210..=239 | 460..=469 | 1010..=1059);
        if float_code && v.trim().parse::<f64>().is_ok_and(|x| !x.is_finite()) {
            return Err(DxfError(format!("bad number at line {n}: {v:?}")));
        }
        out.push(Pair::new(code, v));
    }
    Ok(out)
}

/// Splits at every code 0; the first pair must be code 0.
fn records(body: &[Pair]) -> Result<Vec<&[Pair]>, DxfError> {
    if let Some(p) = body.first() {
        if p.code != 0 {
            return Err(DxfError(format!("expected code 0, found {}", p.code)));
        }
    }
    let mut out = vec![];
    let mut start = 0;
    for i in 1..=body.len() {
        if i == body.len() || body[i].code == 0 {
            out.push(&body[start..i]);
            start = i;
        }
    }
    if body.is_empty() {
        out.clear();
    }
    Ok(out)
}

fn fnum(p: &Pair) -> Result<f64, DxfError> {
    p.value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| DxfError(format!("bad number for code {}: {:?}", p.code, p.value)))
}

fn fint(p: &Pair) -> Result<i32, DxfError> {
    p.value
        .trim()
        .parse()
        .map_err(|_| DxfError(format!("bad integer for code {}: {:?}", p.code, p.value)))
}

fn read_header(body: &[Pair]) -> Vec<HeaderVar> {
    let mut out: Vec<HeaderVar> = vec![];
    for p in body {
        if p.code == 9 {
            out.push(HeaderVar {
                name: p.value.clone(),
                values: vec![],
            });
        } else if let Some(v) = out.last_mut() {
            v.values.push(p.clone());
        }
    }
    out
}

fn parse_layer(rec: &[Pair]) -> Result<Layer, DxfError> {
    let mut l = Layer {
        name: String::new(),
        flags: 0,
        color: 7,
        linetype: "CONTINUOUS".into(),
        extra: vec![],
    };
    for p in &rec[1..] {
        match p.code {
            2 => l.name = p.value.clone(),
            70 => l.flags = fint(p)?,
            62 => l.color = fint(p)?,
            6 => l.linetype = p.value.clone(),
            c if is_structural(c) => {}
            _ => l.extra.push(p.clone()),
        }
    }
    Ok(l)
}

fn parse_ltype(rec: &[Pair]) -> Result<LineType, DxfError> {
    let mut t = LineType {
        name: String::new(),
        description: String::new(),
        flags: 0,
        pattern_length: 0.0,
        pattern: vec![],
        extra: vec![],
    };
    for p in &rec[1..] {
        match p.code {
            2 => t.name = p.value.clone(),
            3 => t.description = p.value.clone(),
            70 => t.flags = fint(p)?,
            40 => t.pattern_length = fnum(p)?,
            49 => t.pattern.push(fnum(p)?),
            72 | 73 => {}
            74 if p.value.trim() == "0" => {}
            c if is_structural(c) => {}
            _ => t.extra.push(p.clone()),
        }
    }
    Ok(t)
}

fn parse_style(rec: &[Pair]) -> Result<TextStyle, DxfError> {
    let mut t = TextStyle {
        name: String::new(),
        flags: 0,
        height: 0.0,
        width_factor: 1.0,
        oblique: 0.0,
        font: String::new(),
        big_font: String::new(),
        extra: vec![],
    };
    for p in &rec[1..] {
        match p.code {
            2 => t.name = p.value.clone(),
            70 => t.flags = fint(p)?,
            40 => t.height = fnum(p)?,
            41 => t.width_factor = fnum(p)?,
            50 => t.oblique = fnum(p)?,
            3 => t.font = p.value.clone(),
            4 => t.big_font = p.value.clone(),
            c if is_structural(c) => {}
            _ => t.extra.push(p.clone()),
        }
    }
    Ok(t)
}

fn read_tables(body: &[Pair], doc: &mut DxfDocument) -> Result<(), DxfError> {
    let recs = records(body)?;
    let mut i = 0;
    while i < recs.len() {
        let head = recs[i];
        if head[0].value != "TABLE" {
            return Err(DxfError(format!("expected TABLE, found {}", head[0].value)));
        }
        let name = head
            .iter()
            .find(|p| p.code == 2)
            .map(|p| p.value.clone())
            .ok_or_else(|| DxfError("TABLE without name".into()))?;
        let end = (i + 1..recs.len())
            .find(|&j| recs[j][0].value == "ENDTAB")
            .ok_or_else(|| DxfError(format!("table {name} not closed")))?;
        let entries = &recs[i + 1..end];
        let typed = matches!(name.as_str(), "LAYER" | "LTYPE" | "STYLE")
            && entries.iter().all(|r| r[0].value == name);
        match (typed, name.as_str()) {
            (true, "LAYER") => {
                for r in entries {
                    doc.layers.push(parse_layer(r)?);
                }
            }
            (true, "LTYPE") => {
                for r in entries {
                    doc.ltypes.push(parse_ltype(r)?);
                }
            }
            (true, "STYLE") => {
                for r in entries {
                    doc.styles.push(parse_style(r)?);
                }
            }
            _ => {
                let mut header = vec![];
                let mut skipped_name = false;
                for p in &head[1..] {
                    if p.code == 2 && !skipped_name {
                        skipped_name = true;
                    } else {
                        header.push(p.clone());
                    }
                }
                doc.other_tables.push(RawTable {
                    name,
                    header,
                    entries: entries.iter().map(|r| r.to_vec()).collect(),
                });
            }
        }
        i = end + 1;
    }
    Ok(())
}

fn finish_block(mut b: Block, inner: Vec<&[Pair]>) -> Result<Block, DxfError> {
    for e in parse_all(&inner)? {
        match e {
            Entity::AttDef(a) => b.attdefs.push(a),
            e => b.entities.push(e),
        }
    }
    Ok(b)
}

fn read_blocks(body: &[Pair], doc: &mut DxfDocument) -> Result<(), DxfError> {
    let recs = records(body)?;
    let mut cur: Option<(Block, Vec<&[Pair]>)> = None;
    for r in recs {
        match r[0].value.as_str() {
            "BLOCK" => {
                let mut b = Block::new("");
                for p in &r[1..] {
                    match p.code {
                        2 => b.name = p.value.clone(),
                        8 => b.layer = p.value.clone(),
                        70 => b.flags = fint(p)?,
                        10 => b.base[0] = fnum(p)?,
                        20 => b.base[1] = fnum(p)?,
                        30 => b.base[2] = fnum(p)?,
                        3 => {}
                        c if is_structural(c) => {}
                        _ => b.extra.push(p.clone()),
                    }
                }
                cur = Some((b, vec![]));
            }
            "ENDBLK" => {
                let (b, inner) = cur
                    .take()
                    .ok_or_else(|| DxfError("ENDBLK without BLOCK".into()))?;
                let b = finish_block(b, inner)?;
                // Re-created by the writer for every file; keeping them would break round trips.
                let space = b.name.eq_ignore_ascii_case("*Model_Space")
                    || b.name.eq_ignore_ascii_case("*Paper_Space");
                if !(space && b.entities.is_empty() && b.attdefs.is_empty()) {
                    doc.blocks.push(b);
                }
            }
            _ => match cur.as_mut() {
                Some((_, inner)) => inner.push(r),
                None => return Err(DxfError("entity outside BLOCK".into())),
            },
        }
    }
    if cur.is_some() {
        return Err(DxfError("BLOCK not closed".into()));
    }
    Ok(())
}

pub fn read_str(text: &str) -> Result<DxfDocument, DxfError> {
    read_str_with(text, &|| false)
}

/// [`read_str`] that polls `stop` while tokenizing and between sections; a stop gives an error
/// whose text is [`crate::job::CANCELLED`].
pub fn read_str_with(text: &str, stop: &dyn Fn() -> bool) -> Result<DxfDocument, DxfError> {
    read_str_at(text, stop, &|_, _, _| {})
}

/// Catalog key of what a DXF section holds, for the progress line.
fn section_key(name: &str) -> &'static str {
    match name {
        "TABLES" => "job.kind.layers",
        "BLOCKS" => "job.kind.blocks",
        "ENTITIES" => "job.kind.entities",
        _ => "job.kind.header",
    }
}

/// [`read_str_with`] telling `at` (pairs done, pairs in all, section) as each section starts.
fn read_str_at(
    text: &str,
    stop: &dyn Fn() -> bool,
    at: &dyn Fn(u64, u64, &'static str),
) -> Result<DxfDocument, DxfError> {
    let pairs = tokenize_with(text, stop)?;
    if pairs.is_empty() {
        return Err(DxfError("empty file: no DXF data".into()));
    }
    let mut doc = DxfDocument::default();
    let mut i = 0;
    while i < pairs.len() {
        if stop() {
            return Err(cancelled());
        }
        if let Some(n) = pairs.get(i + 1).filter(|n| n.code == 2) {
            at(i as u64, pairs.len() as u64, section_key(&n.value));
        }
        let p = &pairs[i];
        if p.code == 0 && p.value == "EOF" {
            break;
        }
        if !(p.code == 0 && p.value == "SECTION") {
            return Err(DxfError(format!(
                "expected SECTION, found {} {:?}",
                p.code, p.value
            )));
        }
        let name = match pairs.get(i + 1) {
            Some(n) if n.code == 2 => n.value.clone(),
            _ => return Err(DxfError("SECTION without name".into())),
        };
        let end = (i + 2..pairs.len())
            .find(|&j| pairs[j].code == 0 && pairs[j].value == "ENDSEC")
            .ok_or_else(|| DxfError(format!("section {name} not closed")))?;
        let body = &pairs[i + 2..end];
        match name.as_str() {
            "HEADER" => doc.header = read_header(body),
            "TABLES" => read_tables(body, &mut doc)?,
            "BLOCKS" => read_blocks(body, &mut doc)?,
            "ENTITIES" => doc.entities = parse_all(&records(body)?)?,
            _ => doc.other_sections.push(RawSection {
                name,
                pairs: body.to_vec(),
            }),
        }
        i = end + 1;
    }
    at(pairs.len() as u64, pairs.len() as u64, "job.kind.entities");
    Ok(doc)
}

/// [`read_file_with`] reporting on `c`: stage `job.stage.read` (bytes, 0–50 %), then
/// `job.stage.parse` (code pairs by section, 50–100 %); `c` also carries the cancel.
/// A .dwg parses in one step: its stage shows without a count.
pub fn read_file_ctl(path: &Path, c: &crate::job::Progress) -> Result<DxfDocument, DxfError> {
    use std::io::Read;
    let io = |e: std::io::Error| DxfError(e.to_string());
    let stop = || c.is_cancelled();
    c.phase("job.stage.read", 0.0, 0.5);
    c.set_bytes(true);
    let mut f = std::fs::File::open(path).map_err(io)?;
    let len = f.metadata().map_or(0, |m| m.len());
    c.set_total(len);
    let mut bytes = Vec::with_capacity(len as usize);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if stop() {
            return Err(cancelled());
        }
        let n = f.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        c.add(n as u64);
    }
    c.phase("job.stage.parse", 0.5, 1.0);
    if crate::dwg::is_dwg(&bytes) {
        return crate::dwg::read_bytes_stop(&bytes, &stop).map_err(|e| DxfError(e.to_string()));
    }
    let text = decode_bytes(&bytes);
    drop(bytes);
    read_str_at(&text, &stop, &|d, t, what| {
        c.set_total(t);
        c.set(d);
        c.set_detail(what);
    })
}

/// Decodes raw DXF bytes: UTF-8 first, else the `$DWGCODEPAGE` single-byte page, else Latin-1.
fn decode_bytes(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    super::codepage::decode(
        bytes,
        header_codepage(bytes).and_then(super::codepage::page),
    )
}

/// Value of `$DWGCODEPAGE` read straight from the bytes (the header is ASCII).
fn header_codepage(bytes: &[u8]) -> Option<&str> {
    const KEY: &[u8] = b"$DWGCODEPAGE";
    let at = bytes.windows(KEY.len()).position(|w| w == KEY)? + KEY.len();
    let head = &bytes[at..bytes.len().min(at + 64)];
    let mut lines = head.split(|&b| b == b'\n').skip(1);
    let code = std::str::from_utf8(lines.next()?).ok()?.trim();
    let val = std::str::from_utf8(lines.next()?).ok()?.trim();
    (code == "3").then_some(val)
}

/// Also accepts .dwg bytes (detected by signature), read natively into the same structure.
pub fn read_bytes(bytes: &[u8]) -> Result<DxfDocument, DxfError> {
    read_bytes_with(bytes, &|| false)
}

/// [`read_bytes`] with a stop check (see [`read_str_with`]); a .dwg checks between its stages.
pub fn read_bytes_with(bytes: &[u8], stop: &dyn Fn() -> bool) -> Result<DxfDocument, DxfError> {
    if crate::dwg::is_dwg(bytes) {
        return crate::dwg::read_bytes_stop(bytes, stop).map_err(|e| DxfError(e.to_string()));
    }
    read_str_with(&decode_bytes(bytes), stop)
}

pub fn read_file(path: &Path) -> Result<DxfDocument, DxfError> {
    read_file_with(path, &|| false)
}

/// [`read_file`] that reads in blocks and polls `stop` all the way (DXF text and .dwg).
pub fn read_file_with(path: &Path, stop: &dyn Fn() -> bool) -> Result<DxfDocument, DxfError> {
    use std::io::Read;
    let io = |e: std::io::Error| DxfError(e.to_string());
    let mut f = std::fs::File::open(path).map_err(io)?;
    let mut bytes = Vec::with_capacity(f.metadata().map_or(0, |m| m.len() as usize));
    let mut buf = vec![0u8; 4 << 20];
    loop {
        if stop() {
            return Err(cancelled());
        }
        let n = f.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    read_bytes_with(&bytes, stop)
}
