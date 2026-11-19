//! Files a person attaches to the AI chat: what kind each one is and what can be read out of it
//! on this machine — text per page, a table (room programme), drawing geometry counts and the
//! sheet scale. `facts` turns that into project requirements, each with its source.

pub mod facts;
#[cfg(test)]
mod tests;

use crate::drawing_model::Underlay;
use crate::pdf::import::{PageDrawing, PathSeg, PdfFile};
use std::io::{Cursor, Read};

/// Files above this are refused before reading.
pub const MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Text kept per page or sheet; the rest is cut and said so.
pub const MAX_PAGE_CHARS: usize = 60_000;
/// Pages and sheets read per file.
pub const MAX_PAGES: usize = 200;
/// Table rows kept from a sheet.
pub const MAX_ROWS: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pdf,
    Image,
    Dxf,
    Dwg,
    Ifc,
    Sheet,
    Csv,
    Text,
    Docx,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Pdf => "pdf",
            Kind::Image => "image",
            Kind::Dxf => "dxf",
            Kind::Dwg => "dwg",
            Kind::Ifc => "ifc",
            Kind::Sheet => "xlsx",
            Kind::Csv => "csv",
            Kind::Text => "text",
            Kind::Docx => "docx",
        }
    }

    /// Drawings that can lie on a level as a snap-able underlay.
    pub fn is_drawing(self) -> bool {
        matches!(self, Kind::Pdf | Kind::Dxf | Kind::Dwg)
    }
}

/// Extensions the attach dialog offers.
pub const EXTENSIONS: &[&str] = &[
    "pdf", "png", "jpg", "jpeg", "dxf", "dwg", "ifc", "xlsx", "xlsm", "csv", "txt", "md", "docx",
];

/// Kind from the name, confirmed by the first bytes where the format has a signature.
pub fn detect(name: &str, bytes: &[u8]) -> Option<Kind> {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if bytes.starts_with(b"%PDF-") {
        return Some(Kind::Pdf);
    }
    if bytes.starts_with(b"\x89PNG") || bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Kind::Image);
    }
    if crate::dwg::is_dwg(bytes) {
        return Some(Kind::Dwg);
    }
    let zip = bytes.starts_with(b"PK\x03\x04");
    match ext.as_str() {
        "xlsx" | "xlsm" if zip => Some(Kind::Sheet),
        "docx" if zip => Some(Kind::Docx),
        "dxf" => Some(Kind::Dxf),
        "ifc" if bytes.starts_with(b"ISO-10303-21") => Some(Kind::Ifc),
        "csv" | "tsv" => Some(Kind::Csv),
        "txt" | "md" => Some(Kind::Text),
        _ => None,
    }
}

/// Where the sheet scale came from.
#[derive(Clone, Debug, PartialEq)]
pub enum ScaleSrc {
    /// A written scale such as "M 1:100".
    Text(String),
    /// Dimension texts against the lines they stand on: how many agreed.
    Dimensions(usize),
}

/// 1:N of a sheet, on one page.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetScale {
    pub ratio: f64,
    pub page: usize,
    pub src: ScaleSrc,
}

/// What was read out of one file. Pages are 0-based here; replies show them 1-based.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extracted {
    /// Text per page (PDF), sheet (workbook) or the whole file (one entry).
    pub pages: Vec<String>,
    /// First table: the sheet of a workbook or a CSV, rows of cell text.
    pub table: Vec<Vec<String>>,
    pub page_count: usize,
    /// Straight segments of drawing geometry (all pages of a PDF).
    pub lines: usize,
    pub scale: Option<SheetScale>,
    /// Storeys and spaces named in an IFC file.
    pub storeys: Vec<String>,
    pub spaces: usize,
    /// Something was cut or skipped: the words say what.
    pub notes: Vec<String>,
}

impl Extracted {
    pub fn chars(&self) -> usize {
        self.pages.iter().map(|p| p.chars().count()).sum()
    }
}

/// Reads what can be read locally. An image gives nothing here: it goes to a model that sees.
pub fn extract(kind: Kind, bytes: &[u8]) -> Result<Extracted, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("file is too big".into());
    }
    match kind {
        Kind::Pdf => pdf(bytes),
        Kind::Image => Ok(Extracted::default()),
        Kind::Dxf | Kind::Dwg => drawing(kind, bytes),
        Kind::Ifc => Ok(ifc(&String::from_utf8_lossy(bytes))),
        Kind::Sheet => sheet(bytes),
        Kind::Csv => Ok(csv(&decode_text(bytes))),
        Kind::Text => Ok(one_page(decode_text(bytes))),
        Kind::Docx => docx(bytes).map(one_page),
    }
}

fn one_page(text: String) -> Extracted {
    let mut e = Extracted {
        page_count: 1,
        ..Default::default()
    };
    e.pages.push(clip(text, &mut e.notes));
    e
}

fn clip(mut s: String, notes: &mut Vec<String>) -> String {
    if s.chars().count() > MAX_PAGE_CHARS {
        let cut = s.char_indices().nth(MAX_PAGE_CHARS).map_or(s.len(), |(i, _)| i);
        s.truncate(cut);
        notes.push(format!("text cut at {MAX_PAGE_CHARS} characters"));
    }
    s
}

/// UTF-8 (with or without BOM), else UTF-16 with BOM, else the Cyrillic single-byte page.
pub fn decode_text(b: &[u8]) -> String {
    if let Some(r) = b.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(r).into_owned();
    }
    if b.starts_with(b"\xFF\xFE") || b.starts_with(b"\xFE\xFF") {
        let enc = if b[0] == 0xFF { encoding_rs::UTF_16LE } else { encoding_rs::UTF_16BE };
        return enc.decode(&b[2..]).0.into_owned();
    }
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1251.decode(b).0.into_owned(),
    }
}

fn pdf(bytes: &[u8]) -> Result<Extracted, String> {
    let doc = PdfFile::parse(bytes).map_err(|e| format!("{e:?}"))?;
    let n = doc.page_count();
    let mut e = Extracted {
        page_count: n,
        ..Default::default()
    };
    if n > MAX_PAGES {
        e.notes.push(format!("only the first {MAX_PAGES} of {n} pages are read"));
    }
    for i in 0..n.min(MAX_PAGES) {
        let page = doc.page(i).map_err(|e| format!("{e:?}"))?;
        let segs = page_segments(&page);
        e.lines += segs.len();
        if e.scale.is_none() {
            e.scale = scale_of(&page, &segs, i);
        }
        let text = page_text(&page);
        e.pages.push(clip(text, &mut e.notes));
    }
    Ok(e)
}

/// Text runs in reading order: top to bottom, then left to right; one line per baseline.
pub fn page_text(page: &PageDrawing) -> String {
    let mut runs: Vec<_> = page.texts.iter().filter(|t| !t.string.trim().is_empty()).collect();
    runs.sort_by(|a, b| {
        b.pos[1]
            .partial_cmp(&a.pos[1])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.pos[0].partial_cmp(&b.pos[0]).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut out = String::new();
    let mut last: Option<f64> = None;
    for t in runs {
        let tol = (t.height_mm * 0.5).max(0.5);
        match last {
            Some(y) if (y - t.pos[1]).abs() <= tol => out.push(' '),
            Some(_) => out.push('\n'),
            None => {}
        }
        out.push_str(t.string.trim());
        last = Some(t.pos[1]);
    }
    out
}

/// Straight stroked segments of a page in sheet millimetres.
pub fn page_segments(page: &PageDrawing) -> Vec<([f64; 2], [f64; 2])> {
    let mut out = vec![];
    for s in page.shapes.iter().filter(|s| s.stroke) {
        for sp in &s.subpaths {
            let mut at = sp.start;
            for seg in &sp.segs {
                match seg {
                    PathSeg::Line(p) => {
                        out.push((at, *p));
                        at = *p;
                    }
                    PathSeg::Cubic(_, _, p) => at = *p,
                }
            }
            if sp.closed && at != sp.start {
                out.push((at, sp.start));
            }
        }
    }
    out
}

/// Scales drawings are made in.
pub const STANDARD_RATIOS: &[f64] = &[
    1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 75.0, 100.0, 200.0, 250.0, 400.0, 500.0, 1000.0,
    2000.0, 5000.0,
];

fn standard(r: f64) -> Option<f64> {
    STANDARD_RATIOS.iter().copied().find(|s| (r / s - 1.0).abs() <= 0.03)
}

/// "1:100", "M 1:50", "scale 1 : 200" (also in Russian) in a text; only standard ratios count.
pub fn scale_in_text(s: &str) -> Option<(f64, String)> {
    let b: Vec<char> = s.chars().collect();
    for i in 0..b.len() {
        if b[i] != '1' || (i > 0 && b[i - 1].is_ascii_digit()) {
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && b[j] == ' ' {
            j += 1;
        }
        if j >= b.len() || b[j] != ':' {
            continue;
        }
        j += 1;
        while j < b.len() && b[j] == ' ' {
            j += 1;
        }
        let k0 = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j == k0 || (j < b.len() && (b[j].is_ascii_digit() || b[j] == ':' || b[j] == '.')) {
            continue;
        }
        let n: f64 = b[k0..j].iter().collect::<String>().parse().ok()?;
        if let Some(r) = STANDARD_RATIOS.iter().copied().find(|x| *x == n && n > 1.0) {
            let from = i.saturating_sub(12);
            return Some((r, b[from..j].iter().collect::<String>().trim().to_string()));
        }
    }
    None
}

/// A written scale wins; else dimension numbers against the lines they label vote.
pub fn scale_of(page: &PageDrawing, segs: &[([f64; 2], [f64; 2])], index: usize) -> Option<SheetScale> {
    for t in &page.texts {
        if let Some((ratio, said)) = scale_in_text(&t.string) {
            return Some(SheetScale {
                ratio,
                page: index,
                src: ScaleSrc::Text(said),
            });
        }
    }
    let mut votes: Vec<(f64, usize)> = vec![];
    for t in &page.texts {
        let s: String = t.string.chars().filter(|c| !c.is_whitespace()).collect();
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(v) = s.parse::<f64>() else { continue };
        if !(100.0..=200_000.0).contains(&v) {
            continue;
        }
        let reach = (t.height_mm * 2.5).max(4.0);
        let best = segs
            .iter()
            .filter_map(|(a, b)| {
                let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
                let axis = (b[0] - a[0]).abs() < 1e-3 || (b[1] - a[1]).abs() < 1e-3;
                if len < 3.0 || !axis {
                    return None;
                }
                let m = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
                let d = ((m[0] - t.pos[0]).powi(2) + (m[1] - t.pos[1]).powi(2)).sqrt();
                (d <= reach + len / 2.0 * 0.2).then_some((d, len))
            })
            .min_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
        if let Some(r) = best.and_then(|(_, len)| standard(v / len)) {
            match votes.iter_mut().find(|(x, _)| *x == r) {
                Some(v) => v.1 += 1,
                None => votes.push((r, 1)),
            }
        }
    }
    votes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)));
    let (ratio, n) = *votes.first()?;
    // one dimension is a guess: two that agree are a scale
    (n >= 2).then_some(SheetScale {
        ratio,
        page: index,
        src: ScaleSrc::Dimensions(n),
    })
}

fn drawing(kind: Kind, bytes: &[u8]) -> Result<Extracted, String> {
    let doc = if kind == Kind::Dwg {
        crate::dwg::read_bytes(bytes).map_err(|e| format!("{e:?}"))?
    } else {
        crate::dxf::read_bytes(bytes).map_err(|e| e.0)?
    };
    let (u, _unit) = Underlay::from_dxf_auto(&doc);
    Ok(from_underlay(&u))
}

pub fn from_underlay(u: &Underlay) -> Extracted {
    let mut e = Extracted {
        page_count: 1,
        lines: u.lines.len(),
        ..Default::default()
    };
    let mut texts: Vec<_> = u.texts.iter().collect();
    texts.sort_by(|a, b| {
        b.pos.1
            .partial_cmp(&a.pos.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.pos.0.partial_cmp(&b.pos.0).unwrap_or(std::cmp::Ordering::Equal))
    });
    let text = texts.iter().map(|t| t.text.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>();
    e.pages.push(clip(text.join("\n"), &mut e.notes));
    e
}

/// Storey and space names out of the STEP text; the model itself is read by the IFC import.
fn ifc(text: &str) -> Extracted {
    let mut e = Extracted {
        page_count: 1,
        ..Default::default()
    };
    for line in text.split(';') {
        let l = line.trim();
        let Some(eq) = l.find('=') else { continue };
        let rec = l[eq + 1..].trim_start();
        let up = rec.get(..rec.find('(').unwrap_or(rec.len())).unwrap_or("").trim();
        if up.eq_ignore_ascii_case("IFCBUILDINGSTOREY") {
            e.storeys.push(step_name(rec).unwrap_or_else(|| "?".into()));
        } else if up.eq_ignore_ascii_case("IFCSPACE") {
            e.spaces += 1;
        }
    }
    let mut t = format!("IFC: {} storeys, {} spaces", e.storeys.len(), e.spaces);
    for s in &e.storeys {
        t.push_str("\nstorey ");
        t.push_str(s);
    }
    e.pages.push(t);
    e
}

/// Third attribute (Name) of a STEP record when it is a string.
fn step_name(rec: &str) -> Option<String> {
    let inner = &rec[rec.find('(')? + 1..];
    let mut field = 0;
    let mut i = 0;
    let b: Vec<char> = inner.chars().collect();
    while i < b.len() {
        match b[i] {
            '\'' => {
                let mut s = String::new();
                i += 1;
                while i < b.len() {
                    if b[i] == '\'' {
                        if b.get(i + 1) == Some(&'\'') {
                            s.push('\'');
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    s.push(b[i]);
                    i += 1;
                }
                if field == 2 {
                    return Some(s);
                }
            }
            ',' => field += 1,
            _ => {}
        }
        i += 1;
    }
    None
}

fn sheet(bytes: &[u8]) -> Result<Extracted, String> {
    use calamine::{Data, Reader, Xlsx};
    let mut wb: Xlsx<_> = Xlsx::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let names = wb.sheet_names();
    let mut e = Extracted {
        page_count: names.len(),
        ..Default::default()
    };
    for name in names.iter().take(MAX_PAGES) {
        let Ok(range) = wb.worksheet_range(name) else { continue };
        let mut rows: Vec<Vec<String>> = vec![];
        for r in range.rows().take(MAX_ROWS) {
            let row: Vec<String> = r
                .iter()
                .map(|c| match c {
                    Data::Empty => String::new(),
                    Data::Float(f) => num_text(*f),
                    Data::Int(i) => i.to_string(),
                    other => other.to_string(),
                })
                .collect();
            rows.push(row);
        }
        if range.rows().count() > MAX_ROWS {
            e.notes.push(format!("sheet {name}: only the first {MAX_ROWS} rows are read"));
        }
        let text = rows.iter().map(|r| r.join("\t")).collect::<Vec<_>>().join("\n");
        e.pages.push(clip(format!("[{name}]\n{text}"), &mut e.notes));
        if e.table.is_empty() && rows.iter().any(|r| r.iter().any(|c| !c.is_empty())) {
            e.table = rows;
        }
    }
    Ok(e)
}

fn num_text(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

/// Separator from the first line: tab, semicolon (comma-decimal locales) or comma.
fn csv(text: &str) -> Extracted {
    let first = text.lines().next().unwrap_or("");
    let sep = ['\t', ';', ','].into_iter().max_by_key(|c| first.matches(*c).count()).unwrap_or(',');
    let mut e = one_page(text.to_string());
    for line in text.lines().take(MAX_ROWS) {
        e.table.push(split_csv(line, sep));
    }
    e
}

fn split_csv(line: &str, sep: char) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' if quoted && it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            '"' => quoted = !quoted,
            c if c == sep && !quoted => out.push(std::mem::take(&mut cur).trim().to_string()),
            c => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

/// Paragraph text of `word/document.xml`; formatting, tables' layout and images are dropped.
fn docx(bytes: &[u8]) -> Result<String, String> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut f = z.by_name("word/document.xml").map_err(|e| e.to_string())?;
    if f.size() > MAX_BYTES {
        return Err("document is too big".into());
    }
    let mut xml = String::new();
    f.read_to_string(&mut xml).map_err(|e| e.to_string())?;
    Ok(xml_text(&xml))
}

/// `<w:t>` runs joined; paragraph and row ends become new lines, cells and tabs a tab.
pub fn xml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    // only text inside <w:t> is content; anything else between tags is markup whitespace
    let mut in_t = false;
    while let Some(lt) = rest.find('<') {
        if in_t {
            out.push_str(&unescape(&rest[..lt]));
        }
        let Some(gt) = rest[lt..].find('>') else { break };
        let tag = &rest[lt + 1..lt + gt];
        let name = tag.trim_start_matches('/').split([' ', '/']).next().unwrap_or("");
        let closing = tag.starts_with('/');
        match name {
            "w:t" => in_t = !closing && !tag.ends_with('/'),
            "w:p" | "w:tr" if closing => out.push('\n'),
            "w:tc" if closing => out.push('\t'),
            "w:tab" => out.push('\t'),
            "w:br" => out.push('\n'),
            _ => {}
        }
        rest = &rest[lt + gt + 1..];
    }
    out.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim().to_string()
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
