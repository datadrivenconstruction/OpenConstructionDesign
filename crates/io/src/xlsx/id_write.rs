// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Ids of new rows written into the book in place: only the id cells of those rows change, and
//! the hidden baseline gets an id-only block naming them (so a row deleted later deletes its
//! element); every other part of the file is copied byte for byte. Used when a full re-export
//! cannot keep the user's layout (a main sheet read under the name the user gave it).
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use calamine::{open_workbook, Data, Range, Reader, Xlsx};

use super::*;

type Book = Xlsx<std::io::BufReader<File>>;

/// Known sheets whose rows can get their id written in place.
pub const ID_SHEETS: [&str; 6] = [LEVELS, WALLS, OPENINGS, ROOMS, SLABS, CURTAINS];

/// Row `row` (1-based) of the known sheet `sheet` gets the element id `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdCell {
    pub sheet: String,
    pub row: u32,
    pub id: i64,
}

fn known(sheet: &str) -> Option<(&'static str, &'static [Col])> {
    [
        (LEVELS, LEVELS_COLS),
        (WALLS, WALLS_COLS),
        (OPENINGS, OPENINGS_COLS),
        (ROOMS, ROOMS_COLS),
        (SLABS, SLABS_COLS),
        (CURTAINS, CURTAINS_COLS),
    ]
    .into_iter()
    .find(|k| k.0 == sheet)
}

/// (sheet of the book, 0-based id column) -> row -> id.
type Plan = BTreeMap<(String, u32), BTreeMap<u32, i64>>;

fn blank(d: Option<&Data>) -> bool {
    match d {
        None | Some(Data::Empty) => true,
        Some(Data::String(s)) => s.trim().is_empty(),
        _ => false,
    }
}

fn plan(src: &Path, alias: &HashMap<String, String>, cells: &[IdCell]) -> Result<Plan, String> {
    let mut wb: Book = open_workbook(src).map_err(|e| format!("cannot open workbook: {e}"))?;
    let mut sheets: HashMap<String, (String, u32, Vec<u32>, Range<Data>)> = HashMap::new();
    let mut out = Plan::new();
    for c in cells {
        if !sheets.contains_key(&c.sheet) {
            let (name, cols) = known(&c.sheet).ok_or_else(|| format!("sheet '{}' has no id column to write", c.sheet))?;
            let (actual, col, rows) = super::import::id_column(&mut wb, name, cols, alias)
                .ok_or_else(|| format!("no id column in the sheet read as '{}'", c.sheet))?;
            let range = wb.worksheet_range(&actual).map_err(|e| format!("{actual}: {e}"))?;
            sheets.insert(c.sheet.clone(), (actual, col, rows, range));
        }
        let (actual, col, rows, range) = &sheets[&c.sheet];
        if c.row == 0 || !rows.contains(&c.row) {
            return Err(format!("row {} of '{actual}' is not a data row", c.row));
        }
        if !blank(range.get_value((c.row - 1, *col))) {
            return Err(format!("row {} of '{actual}' already has an id", c.row));
        }
        if out.entry((actual.clone(), *col)).or_default().insert(c.row, c.id).is_some() {
            return Err(format!("row {} of '{actual}' is given two ids", c.row));
        }
    }
    Ok(out)
}

/// End of the tag that starts at `at` (just past its `>`); quoted values may hold `>`.
fn tag_end(s: &str, at: usize) -> Option<usize> {
    let mut q = 0u8;
    for (i, &b) in s.as_bytes().iter().enumerate().skip(at) {
        if q != 0 {
            if b == q {
                q = 0;
            }
        } else if b == b'"' || b == b'\'' {
            q = b;
        } else if b == b'>' {
            return Some(i + 1);
        }
    }
    None
}

fn tag_name(tag: &str) -> &str {
    let t = tag.strip_prefix('<').unwrap_or(tag);
    let n = t.find(|c: char| c.is_whitespace() || c == '/' || c == '>').unwrap_or(t.len());
    &t[..n]
}

fn attrs(tag: &str) -> Vec<(&str, &str)> {
    let b = tag.as_bytes();
    let ws = |i: usize| i < b.len() && b[i].is_ascii_whitespace();
    let mut out = Vec::new();
    let mut i = 1 + tag_name(tag).len();
    loop {
        while ws(i) {
            i += 1;
        }
        let ns = i;
        while i < b.len() && b[i] != b'=' && !b[i].is_ascii_whitespace() && b[i] != b'>' && b[i] != b'/' {
            i += 1;
        }
        if i == ns {
            return out;
        }
        let name = &tag[ns..i];
        while ws(i) {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            return out;
        }
        i += 1;
        while ws(i) {
            i += 1;
        }
        if i >= b.len() || (b[i] != b'"' && b[i] != b'\'') {
            return out;
        }
        let q = b[i] as char;
        let vs = i + 1;
        let Some(len) = tag[vs..].find(q) else {
            return out;
        };
        out.push((name, &tag[vs..vs + len]));
        i = vs + len + 1;
    }
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    attrs(tag).into_iter().find(|a| a.0 == name).map(|a| a.1)
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", r#"""#)
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Every tag `<name ...>` of `xml`, as text.
fn tags<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}");
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = xml[at..].find(&open).map(|i| i + at) {
        let Some(e) = tag_end(xml, i) else {
            break;
        };
        if tag_name(&xml[i..e]) == name {
            out.push(&xml[i..e]);
        }
        at = e;
    }
    out
}

/// 0-based column letters: 0 -> A, 26 -> AA.
pub(crate) fn col_name(col: u32) -> String {
    let mut c = col + 1;
    let mut s = Vec::new();
    while c > 0 {
        s.push(b'A' + ((c - 1) % 26) as u8);
        c = (c - 1) / 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// "AB12" -> (27, 12).
fn parse_ref(r: &str) -> Option<(u32, u32)> {
    let k = r.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = r.split_at(k);
    if letters.is_empty() || !letters.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let col = letters.bytes().try_fold(0u32, |a, b| a.checked_mul(26)?.checked_add(u32::from(b - b'A') + 1))?;
    Some((col - 1, digits.parse().ok()?))
}

fn read_part(z: &mut zip::ZipArchive<File>, name: &str) -> Result<String, String> {
    let mut f = z.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    let mut s = String::new();
    f.read_to_string(&mut s).map_err(|e| format!("{name}: {e}"))?;
    Ok(s)
}

/// Zip part of the worksheet `sheet`, from the workbook and its relations.
fn sheet_part(z: &mut zip::ZipArchive<File>, sheet: &str) -> Result<String, String> {
    let wb = read_part(z, "xl/workbook.xml")?;
    let rid = tags(&wb, "sheet")
        .into_iter()
        .find(|t| attr(t, "name").is_some_and(|n| unescape(n) == sheet))
        .and_then(|t| attrs(t).into_iter().find(|a| a.0.ends_with(":id")).map(|a| a.1.to_string()))
        .ok_or_else(|| format!("sheet '{sheet}' not found in the workbook"))?;
    let rels = read_part(z, "xl/_rels/workbook.xml.rels")?;
    let target = tags(&rels, "Relationship")
        .into_iter()
        .find(|t| attr(t, "Id") == Some(rid.as_str()))
        .and_then(|t| attr(t, "Target"))
        .ok_or_else(|| format!("sheet '{sheet}' has no part"))?;
    Ok(match target.strip_prefix('/') {
        Some(abs) => abs.to_string(),
        None => format!("xl/{}", target.trim_start_matches("./")),
    })
}

/// The row tag opened for content: a self-closed row gets a body, a `spans` hint that leaves
/// out the id column goes.
fn row_head(tag: &str, col: u32) -> String {
    let mut h = tag.to_string();
    if let Some(sp) = attr(tag, "spans") {
        let inside = sp
            .split_once(':')
            .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)))
            .is_some_and(|(a, b)| a <= col + 1 && col < b);
        if !inside {
            h = h.replacen(&format!(r#" spans="{sp}""#), "", 1);
        }
    }
    if let Some(open) = h.strip_suffix("/>") {
        h = format!("{}>", open.trim_end());
    }
    h
}

/// The row body with the id cell of column `col` set to `id`; its style stays.
fn put_cell(body: &str, row: u32, col: u32, id: i64) -> Result<String, String> {
    let cref = format!("{}{row}", col_name(col));
    let mut cells: Vec<(usize, usize, u32, Option<String>)> = Vec::new();
    let mut at = 0;
    while let Some(s) = body[at..].find('<').map(|i| i + at) {
        let e = tag_end(body, s).ok_or("unterminated cell")?;
        let t = &body[s..e];
        if tag_name(t) != "c" {
            return Err(format!("row {row}: unexpected <{}>", tag_name(t)));
        }
        let (c, r) = attr(t, "r").and_then(parse_ref).ok_or_else(|| format!("row {row}: cell without a reference"))?;
        if r != row {
            return Err(format!("row {row}: cell {c} names row {r}"));
        }
        let end = if t.ends_with("/>") {
            e
        } else {
            body[e..].find("</c>").map(|i| i + e + 4).ok_or_else(|| format!("row {row}: unterminated cell"))?
        };
        cells.push((s, end, c, attr(t, "s").map(str::to_string)));
        at = end;
    }
    let cell = |style: Option<&str>| match style {
        Some(st) => format!(r#"<c r="{cref}" s="{st}"><v>{id}</v></c>"#),
        None => format!(r#"<c r="{cref}"><v>{id}</v></c>"#),
    };
    Ok(match cells.iter().find(|x| x.2 == col) {
        Some((s, e, _, style)) => {
            if body[*s..*e].contains("<f") {
                return Err(format!("row {row}: the id cell holds a formula"));
            }
            format!("{}{}{}", &body[..*s], cell(style.as_deref()), &body[*e..])
        }
        None => {
            let pos = cells.iter().find(|x| x.2 > col).map_or(body.len(), |x| x.0);
            format!("{}{}{}", &body[..pos], cell(None), &body[pos..])
        }
    })
}

/// The worksheet xml with the ids of `rows` in column `col`; Err for a layout it does not know.
pub(crate) fn patch(xml: &str, col: u32, rows: &BTreeMap<u32, i64>) -> Result<String, String> {
    let s0 = xml.find("<sheetData").ok_or("no sheet data")?;
    let open = tag_end(xml, s0).ok_or("no sheet data")?;
    if tag_name(&xml[s0..open]) != "sheetData" || xml[s0..open].ends_with("/>") {
        return Err("no sheet data".into());
    }
    let close = xml[open..].find("</sheetData>").map(|i| i + open).ok_or("no sheet data end")?;
    let mut out = String::with_capacity(xml.len() + rows.len() * 40);
    out.push_str(&xml[..open]);
    let mut at = open;
    let mut done = 0usize;
    while let Some(s) = xml[at..close].find('<').map(|i| i + at) {
        let e = tag_end(xml, s).filter(|e| *e <= close).ok_or("unterminated row")?;
        let tag = &xml[s..e];
        if tag_name(tag) != "row" {
            return Err(format!("unexpected <{}> in sheet data", tag_name(tag)));
        }
        let r: u32 = attr(tag, "r").and_then(|v| v.parse().ok()).ok_or("row without a number")?;
        let (body_end, end) = if tag.ends_with("/>") {
            (e, e)
        } else {
            let k = xml[e..close].find("</row>").map(|i| i + e).ok_or("unterminated row")?;
            (k, k + 6)
        };
        out.push_str(&xml[at..s]);
        match rows.get(&r) {
            None => out.push_str(&xml[s..end]),
            Some(id) => {
                out.push_str(&row_head(tag, col));
                out.push_str(&put_cell(&xml[e..body_end], r, col, *id)?);
                out.push_str("</row>");
                done += 1;
            }
        }
        at = end;
    }
    if done != rows.len() {
        return Err("a row to fill is not in the sheet".into());
    }
    out.push_str(&xml[at..]);
    Ok(out)
}

/// Ids the baseline lacks, per known sheet. Without them the import takes the element for one
/// made after the export, and deleting its row later would not delete it.
fn base_adds(src: &Path, cells: &[IdCell]) -> Result<BTreeMap<String, Vec<i64>>, String> {
    let mut wb: Book = open_workbook(src).map_err(|e| format!("cannot open workbook: {e}"))?;
    let mut out: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    // no (intact) baseline: the import never deletes by it, nothing to add
    let Some(base) = super::baseline::read(&mut wb) else {
        return Ok(out);
    };
    for c in cells {
        if base.has_sheet(&c.sheet) && !base.has_id(&c.sheet, c.id) {
            out.entry(c.sheet.clone()).or_default().push(c.id);
        }
    }
    for v in out.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    Ok(out)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Last row an xlsx sheet can hold.
const MAX_ROW: u32 = 1_048_576;

/// The baseline sheet xml with an id-only block per sheet of `adds` after its last row (inline
/// strings, so the shared strings stay as they are), and the cells it adds (0-based).
pub(crate) fn append_base(xml: &str, adds: &BTreeMap<String, Vec<i64>>) -> Result<(String, Vec<((u32, u32), Data)>), String> {
    let s0 = xml.find("<sheetData").ok_or("no baseline data")?;
    let open = tag_end(xml, s0).ok_or("no baseline data")?;
    if tag_name(&xml[s0..open]) != "sheetData" || xml[s0..open].ends_with("/>") {
        return Err("no baseline data".into());
    }
    let close = xml[open..].find("</sheetData>").map(|i| i + open).ok_or("no baseline data end")?;
    let mut last = 0u32;
    for t in tags(&xml[open..close], "row") {
        let r: u32 = attr(t, "r").and_then(|v| v.parse().ok()).ok_or("baseline row without a number")?;
        last = last.max(r);
    }
    let inline = |r: u32, c: u32, t: &str| format!(r#"<c r="{}{r}" t="inlineStr"><is><t>{}</t></is></c>"#, col_name(c), escape(t));
    let mut rows = String::new();
    let mut cells = Vec::new();
    let mut r = last;
    for (sheet, ids) in adds {
        r += 1;
        let head = [(0u32, "#"), (1, sheet.as_str()), (2, super::baseline::ID_ONLY)];
        rows.push_str(&format!(r#"<row r="{r}">"#));
        for (c, t) in head {
            rows.push_str(&inline(r, c, t));
            cells.push(((r - 1, c), Data::String(t.to_string())));
        }
        rows.push_str("</row>");
        for id in ids {
            r += 1;
            rows.push_str(&format!(r#"<row r="{r}"><c r="A{r}"><v>{id}</v></c></row>"#));
            cells.push(((r - 1, 0), Data::Float(*id as f64)));
        }
    }
    if r > MAX_ROW {
        return Err("the baseline sheet is full".into());
    }
    let mut out = String::with_capacity(xml.len() + rows.len());
    let mut from = 0;
    // the used-range hint grows with the rows (only its usual `A1:X9` form is touched)
    if let Some(d) = xml[..s0].find("<dimension").filter(|_| r > last) {
        let e = tag_end(xml, d).ok_or("no baseline data")?;
        let tag = &xml[d..e];
        let grown = attr(tag, "ref").and_then(|rf| {
            let (a, b) = rf.split_once(':')?;
            let (c, _) = parse_ref(b)?;
            Some(tag.replacen(&format!(r#"ref="{rf}""#), &format!(r#"ref="{a}:{}{r}""#, col_name(c.max(2))), 1))
        });
        if let Some(g) = grown {
            out.push_str(&xml[..d]);
            out.push_str(&g);
            from = e;
        }
    }
    out.push_str(&xml[from..close]);
    out.push_str(&rows);
    out.push_str(&xml[close..]);
    Ok((out, cells))
}

/// Every zip entry but `parts` is the same, compressed bytes and checksum, in the same order.
fn same_entries(src: &Path, dst: &Path, parts: &HashMap<String, String>) -> Result<(), String> {
    let mut a = zip::ZipArchive::new(File::open(src).map_err(text)?).map_err(text)?;
    let mut b = zip::ZipArchive::new(File::open(dst).map_err(text)?).map_err(text)?;
    if a.len() != b.len() {
        return Err("written workbook has other parts".into());
    }
    for i in 0..a.len() {
        let (na, ca, ba) = raw_entry(&mut a, i)?;
        let (nb, cb, bb) = raw_entry(&mut b, i)?;
        if na != nb || (!parts.contains_key(&na) && (ca != cb || ba != bb)) {
            return Err(format!("part '{na}' is not copied as it was"));
        }
    }
    Ok(())
}

/// (name, crc32, compressed bytes) of entry `i`.
fn raw_entry(z: &mut zip::ZipArchive<File>, i: usize) -> Result<(String, u32, Vec<u8>), String> {
    let mut f = z.by_index_raw(i).map_err(text)?;
    let mut v = Vec::new();
    f.read_to_end(&mut v).map_err(text)?;
    Ok((f.name().map_err(text)?.into_owned(), f.crc32(), v))
}

fn text<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn rewrite(src: &Path, dst: &Path, parts: &HashMap<String, String>) -> Result<(), String> {
    let mut z = zip::ZipArchive::new(File::open(src).map_err(text)?).map_err(text)?;
    let out = File::create(dst).map_err(text)?;
    let mut w = zip::ZipWriter::new(std::io::BufWriter::new(out));
    for i in 0..z.len() {
        let name = z.by_index_raw(i).map_err(text)?.name().map_err(text)?.into_owned();
        match parts.get(&name) {
            Some(xml) => {
                let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
                w.start_file(name.as_str(), o).map_err(text)?;
                w.write_all(xml.as_bytes()).map_err(text)?;
            }
            None => {
                let f = z.by_index_raw(i).map_err(text)?;
                w.raw_copy_file(f).map_err(text)?;
            }
        }
    }
    let mut inner = w.finish().map_err(text)?;
    inner.flush().map_err(text)?;
    Ok(())
}

/// `dst` reads as `src` everywhere but the planned cells, which hold their ids, and the cells
/// `base` appended to the baseline sheet.
fn verify(src: &Path, dst: &Path, plan: &Plan, base: &[((u32, u32), Data)]) -> Result<(), String> {
    let mut a: Book = open_workbook(src).map_err(|e| format!("cannot open workbook: {e}"))?;
    let mut b: Book = open_workbook(dst).map_err(|e| format!("written workbook does not open: {e}"))?;
    let names = a.sheet_names();
    if names != b.sheet_names() {
        return Err("written workbook has other sheets".into());
    }
    for n in &names {
        let (ra, rb) = match (a.worksheet_range(n), b.worksheet_range(n)) {
            (Ok(ra), Ok(rb)) => (ra, rb),
            (Err(_), Err(_)) => continue,
            _ => return Err(format!("sheet '{n}' does not read back")),
        };
        let mut ra = ra;
        for ((_, col), rows) in plan.iter().filter(|p| p.0 .0 == *n) {
            for (r, id) in rows {
                let pos = (r - 1, *col);
                let got = rb.get_value(pos).cloned().unwrap_or(Data::Empty);
                let ok = match &got {
                    Data::Float(x) => (*x - *id as f64).abs() < 0.5,
                    Data::Int(x) => x == id,
                    _ => false,
                };
                let inside = ra.start().is_some_and(|s| s.0 <= pos.0 && s.1 <= pos.1);
                if !ok || !inside {
                    return Err(format!("id {id} does not read back in '{n}' row {r}"));
                }
                ra.set_value(pos, got);
            }
        }
        if n == BASELINE {
            for (pos, want) in base {
                let got = rb.get_value(*pos).cloned().unwrap_or(Data::Empty);
                let ok = match (want, &got) {
                    (Data::String(a), Data::String(b)) => a == b,
                    (Data::Float(a), Data::Float(b)) => a == b,
                    (Data::Float(a), Data::Int(b)) => *a == *b as f64,
                    _ => false,
                };
                let inside = ra.start().is_some_and(|s| s.0 <= pos.0 && s.1 <= pos.1);
                if !ok || !inside {
                    return Err(format!("baseline cell {}{} does not read back", col_name(pos.1), pos.0 + 1));
                }
                ra.set_value(*pos, got);
            }
        }
        // set_value past the end over-allocates by whole rows; those blanks are not cells
        let cells_differ = ra.rows().take(rb.height()).ne(rb.rows());
        if ra.start() != rb.start() || ra.end() != rb.end() || cells_differ {
            return Err(format!("sheet '{n}' differs beyond the id cells"));
        }
    }
    Ok(())
}

/// The import reads every added id from the written baseline.
fn base_reads_back(dst: &Path, adds: &BTreeMap<String, Vec<i64>>) -> Result<(), String> {
    if adds.is_empty() {
        return Ok(());
    }
    let mut wb: Book = open_workbook(dst).map_err(|e| format!("written workbook does not open: {e}"))?;
    let base = super::baseline::read(&mut wb).ok_or("written baseline does not read back")?;
    for (sheet, ids) in adds {
        if let Some(id) = ids.iter().find(|id| !base.has_id(sheet, **id)) {
            return Err(format!("id {id} is not in the written baseline of '{sheet}'"));
        }
    }
    Ok(())
}

/// Writes `cells` into the id column of the book `src` as the new file `dst` (ids the baseline
/// lacks are added to it) and reads it back. Refused (Err, `dst` removed) when a cell is not blank, a row is not one the import reads, the
/// sheet xml has a layout this writer does not know, or the result differs anywhere else.
pub fn write_ids(src: &Path, dst: &Path, alias: &HashMap<String, String>, cells: &[IdCell]) -> Result<usize, String> {
    let plan = plan(src, alias, cells)?;
    let adds = base_adds(src, cells)?;
    let mut z = zip::ZipArchive::new(File::open(src).map_err(text)?).map_err(text)?;
    let mut parts = HashMap::new();
    for ((sheet, col), rows) in &plan {
        let part = sheet_part(&mut z, sheet)?;
        let xml = match parts.remove(&part) {
            Some(x) => x,
            None => read_part(&mut z, &part)?,
        };
        parts.insert(part, patch(&xml, *col, rows)?);
    }
    let mut base_cells = Vec::new();
    if !adds.is_empty() {
        let part = sheet_part(&mut z, BASELINE)?;
        if parts.contains_key(&part) {
            return Err("the baseline shares a part with a sheet being written".into());
        }
        let (xml, added) = append_base(&read_part(&mut z, &part)?, &adds)?;
        parts.insert(part, xml);
        base_cells = added;
    }
    drop(z);
    let r = rewrite(src, dst, &parts)
        .and_then(|_| same_entries(src, dst, &parts))
        .and_then(|_| verify(src, dst, &plan, &base_cells))
        .and_then(|_| base_reads_back(dst, &adds));
    if let Err(e) = r {
        let _ = std::fs::remove_file(dst);
        return Err(e);
    }
    Ok(plan.values().map(BTreeMap::len).sum())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{LayerSpec, Model, WallSpec};
    use rust_xlsxwriter::Workbook;

    type Grid = Vec<Vec<Data>>;

    const MINE: &str = "My walls";

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join("ddc_xlsx_tests");
        std::fs::create_dir_all(&d).unwrap();
        d.join(format!("{}_idw_{name}.xlsx", std::process::id()))
    }

    fn model() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m.add_wall_type("ext", &[LayerSpec { material: "brick".into(), thickness: 0.2 }]).unwrap();
        for (a, b) in [((0., 0.), (5., 0.)), ((5., 0.), (5., 4.))] {
            m.add_wall(&WallSpec { a, b, level: lv, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None }).unwrap();
        }
        m
    }

    fn grids(p: &Path) -> Vec<(String, Grid)> {
        let mut wb: Book = open_workbook(p).unwrap();
        wb.sheet_names()
            .into_iter()
            .map(|n| {
                let g: Grid = wb.worksheet_range(&n).unwrap().rows().map(|r| r.to_vec()).collect();
                (n, g)
            })
            .collect()
    }

    fn save(dst: &Path, sheets: &[(String, Grid)]) {
        let mut out = Workbook::new();
        for (name, grid) in sheets {
            let ws = out.add_worksheet();
            ws.set_name(name).unwrap();
            for (i, row) in grid.iter().enumerate() {
                for (j, d) in row.iter().enumerate() {
                    let (i, j) = (i as u32, j as u16);
                    match d {
                        Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                        Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                        Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                        _ => Ok(()),
                    }
                    .unwrap();
                }
            }
        }
        out.save(dst).unwrap();
    }

    /// Exported book with Walls renamed `MINE`, one id-less row appended (book row 4) and a
    /// user sheet "Notes".
    fn book(tag: &str) -> std::path::PathBuf {
        let m = model();
        let src = tmp(&format!("{tag}_src"));
        export_xlsx(&m, &src).unwrap();
        let mut sheets = grids(&src);
        for (n, g) in &mut sheets {
            if n.as_str() == WALLS {
                let mut extra = g[1].clone();
                extra[0] = Data::Empty;
                g.push(extra);
                *n = MINE.to_string();
            }
        }
        sheets.push(("Notes".into(), vec![vec![Data::String("keep me".into())]]));
        let p = tmp(tag);
        save(&p, &sheets);
        p
    }

    fn alias() -> HashMap<String, String> {
        HashMap::from([(WALLS.to_string(), MINE.to_string())])
    }

    fn cell(row: u32, id: i64) -> IdCell {
        IdCell { sheet: WALLS.into(), row, id }
    }

    fn raw_parts(p: &Path) -> Vec<(String, Vec<u8>)> {
        let mut z = zip::ZipArchive::new(File::open(p).unwrap()).unwrap();
        (0..z.len())
            .map(|i| {
                let mut f = z.by_index(i).unwrap();
                let mut b = Vec::new();
                f.read_to_end(&mut b).unwrap();
                (f.name().unwrap().into_owned(), b)
            })
            .collect()
    }

    #[test]
    fn id_goes_into_the_renamed_sheet_and_nothing_else_changes() {
        let p = book("ok");
        let q = tmp("ok_out");
        assert_eq!(write_ids(&p, &q, &alias(), &[cell(4, 77)]).unwrap(), 1);
        let (a, b) = (grids(&p), grids(&q));
        assert_eq!(a.iter().map(|s| &s.0).collect::<Vec<_>>(), b.iter().map(|s| &s.0).collect::<Vec<_>>());
        assert!(!b.iter().any(|s| s.0 == WALLS), "no fresh known sheet beside the user's");
        for ((n, ga), (_, gb)) in a.iter().zip(&b) {
            if n == MINE {
                assert!(matches!(gb[3][0], Data::Float(x) if x == 77.0), "{:?}", gb[3]);
                assert_eq!(ga[3][1..], gb[3][1..]);
                assert_eq!(ga[..3], gb[..3]);
            } else if n == BASELINE {
                // the export's rows as they were, then an id-only block naming the new id
                assert_eq!(gb.len(), ga.len() + 2);
                assert_eq!(ga[..], gb[..ga.len()]);
                let head: Vec<String> = gb[ga.len()][..3].iter().map(|d| d.to_string()).collect();
                assert_eq!(head, ["#", WALLS, "id"]);
                assert_eq!(gb[ga.len() + 1][0].to_string(), "77");
            } else {
                assert_eq!(ga, gb, "sheet {n} untouched");
            }
        }
        // only the worksheet and the baseline parts were rewritten; styles, strings, the rest
        // are the same bytes
        let (ra, rb) = (raw_parts(&p), raw_parts(&q));
        assert_eq!(ra.len(), rb.len());
        let changed: Vec<&String> = ra.iter().zip(&rb).filter(|(x, y)| x != y).map(|(x, _)| &x.0).collect();
        assert_eq!(changed.len(), 2, "{changed:?}");
        assert!(changed.iter().all(|c| c.starts_with("xl/worksheets/")), "{changed:?}");
        let mut wb: Book = open_workbook(&q).unwrap();
        assert!(super::super::baseline::read(&mut wb).unwrap().has_id(WALLS, 77));
    }

    #[test]
    fn written_id_is_in_the_baseline_so_a_deleted_row_deletes_its_element() {
        let p = book("del");
        let mut m = model();
        // the wall an Apply made from book row 4, after the export
        let (lv, t): (i64, i64) =
            m.sql().query_row("SELECT level, wtype FROM wall LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        let spec = WallSpec { a: (0., 4.), b: (5., 4.), level: lv, base_offset: 0.0, height: 3.0, wall_type: t, offset0: None };
        let w3 = m.add_wall(&spec).unwrap();
        let q = tmp("del_out");
        assert_eq!(write_ids(&p, &q, &alias(), &[cell(4, w3)]).unwrap(), 1);
        let dec = KeyDecisions { alias: alias(), ..Default::default() };
        let look = ImportOptions { apply: false, delete_missing: false, new_params: false };
        // the row is the element: nothing added, nothing missing
        let rep = import_xlsx_keyed(&mut m, &q, look, &dec).unwrap();
        assert!(rep.removed.is_empty() && rep.counts().added == 0, "{:?} {:?}", rep.removed, rep.changes);
        // the user deletes the row: the element is listed as removed from the book
        let mut sheets = grids(&q);
        let mut no_block = sheets.clone();
        for (n, g) in sheets.iter_mut() {
            if n.as_str() == MINE {
                g.remove(3);
            }
        }
        let r = tmp("del_row");
        save(&r, &sheets);
        let rep = import_xlsx_keyed(&mut m, &r, look, &dec).unwrap();
        assert!(rep.removed.iter().any(|x| x.sheet == WALLS && x.id == w3), "{:?}", rep.removed);
        // without the baseline block the same book would keep the element (made after the export)
        for (n, g) in no_block.iter_mut() {
            if n.as_str() == MINE {
                g.remove(3);
            }
            if n.as_str() == BASELINE {
                g.truncate(g.len() - 2);
            }
        }
        save(&r, &no_block);
        let rep = import_xlsx_keyed(&mut m, &r, look, &dec).unwrap();
        assert!(!rep.removed.iter().any(|x| x.id == w3), "{:?}", rep.removed);
    }

    #[test]
    fn baseline_rows_go_after_the_last_row_as_inline_text() {
        let x = r#"<worksheet><dimension ref="A1:E3"/><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row><row r="3"><c r="A3"><v>5</v></c></row></sheetData></worksheet>"#;
        let adds = BTreeMap::from([("A&B".to_string(), vec![7, 9])]);
        let (out, cells) = append_base(x, &adds).unwrap();
        assert!(out.contains(r#"<dimension ref="A1:E6"/>"#), "{out}");
        let rows = r#"<row r="4"><c r="A4" t="inlineStr"><is><t>#</t></is></c><c r="B4" t="inlineStr"><is><t>A&amp;B</t></is></c><c r="C4" t="inlineStr"><is><t>id</t></is></c></row><row r="5"><c r="A5"><v>7</v></c></row><row r="6"><c r="A6"><v>9</v></c></row></sheetData>"#;
        assert!(out.contains(rows), "{out}");
        assert!(out.starts_with(r#"<worksheet><dimension ref="A1:E6"/><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row><row r="3">"#));
        assert_eq!(cells.len(), 5);
        assert_eq!(cells[1], ((3, 1), Data::String("A&B".into())));
        assert_eq!(cells[4], ((5, 0), Data::Float(9.0)));
        assert!(append_base("<sheetData/>", &adds).is_err(), "no rows to follow");
        assert!(append_base(r#"<sheetData><row><c r="A1"/></row></sheetData>"#, &adds).is_err(), "row without a number");
    }

    #[test]
    fn id_only_block_joins_the_sheet_and_keeps_its_values() {
        let p = book("block");
        let q = tmp("block_out");
        write_ids(&p, &q, &alias(), &[cell(4, 9001)]).unwrap();
        let (mut a, mut b): (Book, Book) = (open_workbook(&p).unwrap(), open_workbook(&q).unwrap());
        let (ba, bb) = (super::super::baseline::read(&mut a).unwrap(), super::super::baseline::read(&mut b).unwrap());
        assert_eq!(bb.ids(WALLS).len(), ba.ids(WALLS).len() + 1);
        for id in ba.ids(WALLS) {
            assert_eq!(ba.get(WALLS, id, "height"), bb.get(WALLS, id, "height"), "exported values stay");
        }
        assert!(ba.get(WALLS, ba.ids(WALLS)[0], "height").is_some());
        assert_eq!(bb.get(WALLS, 9001, "height"), Some(&super::super::baseline::BVal::Any), "not compared");
        // a second write appends a second block; both join
        let r = tmp("block_out2");
        let mut g = grids(&q);
        for (n, s) in g.iter_mut() {
            if n.as_str() == MINE {
                let mut extra = s[3].clone();
                extra[0] = Data::Empty;
                s.push(extra);
            }
        }
        save(&r, &g);
        let r2 = tmp("block_out3");
        write_ids(&r, &r2, &alias(), &[cell(5, 9002)]).unwrap();
        let mut c: Book = open_workbook(&r2).unwrap();
        let bc = super::super::baseline::read(&mut c).unwrap();
        assert!(bc.has_id(WALLS, 9001) && bc.has_id(WALLS, 9002));
        assert_eq!(bc.ids(WALLS).len(), ba.ids(WALLS).len() + 2);
    }

    #[test]
    fn id_cell_with_a_value_is_refused_and_nothing_is_written() {
        let p = book("full");
        let q = tmp("full_out");
        let _ = std::fs::remove_file(&q);
        let e = write_ids(&p, &q, &alias(), &[cell(2, 77)]).unwrap_err();
        assert!(e.contains("already has an id"), "{e}");
        assert!(!q.exists());
        let e = write_ids(&p, &q, &alias(), &[cell(40, 77)]).unwrap_err();
        assert!(e.contains("not a data row"), "{e}");
        assert!(!q.exists());
    }

    #[test]
    fn sheet_without_an_id_column_is_refused() {
        let p = book("noid");
        let q = tmp("noid_out");
        let c = IdCell { sheet: LAYERS.into(), row: 2, id: 1 };
        assert!(write_ids(&p, &q, &alias(), &[c]).is_err());
        assert!(!q.exists());
    }

    fn one(r: u32, id: i64) -> BTreeMap<u32, i64> {
        BTreeMap::from([(r, id)])
    }

    #[test]
    fn patch_inserts_in_column_order_and_keeps_the_style() {
        let x = r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1"><v>2</v></c></row><row r="2" spans="2:3"><c r="C2"><v>5</v></c></row><row r="3"/><row r="4"><c r="B4" s="7" t="s"><v>3</v></c></row></sheetData></worksheet>"#;
        let p = patch(x, 1, &one(2, 9)).unwrap();
        assert!(p.contains(r#"<row r="2" spans="2:3"><c r="B2"><v>9</v></c><c r="C2">"#), "{p}");
        let p = patch(x, 0, &one(2, 9)).unwrap();
        assert!(p.contains(r#"<row r="2"><c r="A2"><v>9</v></c><c r="C2">"#), "spans hint dropped: {p}");
        let p = patch(x, 1, &one(3, 9)).unwrap();
        assert!(p.contains(r#"<row r="3"><c r="B3"><v>9</v></c></row>"#), "{p}");
        let p = patch(x, 1, &one(4, 9)).unwrap();
        assert!(p.contains(r#"<row r="4"><c r="B4" s="7"><v>9</v></c></row>"#), "{p}");
        assert!(patch(x, 1, &one(9, 9)).is_err(), "a missing row is refused");
    }

    #[test]
    fn patch_refuses_what_it_cannot_place() {
        let f = r#"<sheetData><row r="2"><c r="A2"><f>1+1</f><v>2</v></c></row></sheetData>"#;
        assert!(patch(f, 0, &one(2, 9)).is_err(), "formula id cell");
        let n = r#"<sheetData><row><c r="A2"/></row></sheetData>"#;
        assert!(patch(n, 0, &one(2, 9)).is_err(), "row without a number");
        let c = r#"<sheetData><row r="2"><c><v>1</v></c></row></sheetData>"#;
        assert!(patch(c, 0, &one(2, 9)).is_err(), "cell without a reference");
        let p = r#"<x:sheetData><x:row r="2"/></x:sheetData>"#;
        assert!(patch(p, 0, &one(2, 9)).is_err(), "prefixed layout");
    }

    #[test]
    fn column_letters_round_trip() {
        for c in [0u32, 1, 25, 26, 27, 701, 702] {
            assert_eq!(parse_ref(&format!("{}5", col_name(c))), Some((c, 5)));
        }
        assert_eq!(col_name(26), "AA");
    }
}
