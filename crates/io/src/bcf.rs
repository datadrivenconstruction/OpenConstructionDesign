//! BCF 2.1 and 3.0 (buildingSMART) issue exchange: a zip of `bcf.version` and one folder per
//! topic with `markup.bcf`, `viewpoint.bcfv` and `snapshot.png`. Export writes the XML by hand;
//! import reads it with a small tolerant reader and matches linked components to model elements
//! by IFC guid (`ext_ref`), reporting those that match nothing. Entries are checked before use:
//! absolute paths, `..`, drive letters and oversize entries refuse the whole file.

mod write;
mod xml;

#[cfg(test)]
mod schema;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Cursor, Read};

use ddc_model::{
    ClipPlane, Model, Topic, TopicBcf, TopicComment, TopicDoc, TopicDocFile, TopicElem,
    TopicImport, Viewpoint, SNAPSHOT_MAX, TOPIC_DOC_MAX,
};
pub use write::export_bytes;
use xml::Node;

/// Written as `OriginatingSystem` so a reader of the same project can match elements that have
/// no IFC guid by their table and id.
const ORIGIN: &str = "ddc-editor";
/// `AuthoringToolId` prefix of a guid that is not an IFC GlobalId.
const GUID_TAG: &str = "guid:";
/// 3.0 author of an issue that has none; read back as empty.
const NO_AUTHOR: &str = "unknown";
const MAX_ENTRIES: usize = 50_000;
const MAX_ENTRY_BYTES: u64 = 64 << 20;
const MAX_TOTAL_BYTES: u64 = 256 << 20;
const MAX_TOPICS: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BcfVersion {
    V21,
    V30,
}

impl BcfVersion {
    fn id(self) -> &'static str {
        match self {
            BcfVersion::V21 => "2.1",
            BcfVersion::V30 => "3.0",
        }
    }
}

#[derive(Debug)]
pub enum BcfError {
    /// Not a readable zip, or an entry that cannot be read.
    Zip(String),
    /// An entry name that would leave the archive folder; nothing was imported.
    UnsafePath(String),
    TooLarge(String),
    Xml(String),
    /// Missing or unsupported `bcf.version`.
    Version(String),
    Model(ddc_model::Error),
    Io(std::io::Error),
}

impl fmt::Display for BcfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BcfError::Zip(e) => write!(f, "not a readable BCF zip: {e}"),
            BcfError::UnsafePath(p) => write!(f, "unsafe path in the archive: {p}"),
            BcfError::TooLarge(p) => write!(f, "archive entry too large: {p}"),
            BcfError::Xml(e) => write!(f, "bad XML: {e}"),
            BcfError::Version(v) => write!(f, "unsupported BCF version: {v}"),
            BcfError::Model(e) => write!(f, "{e}"),
            BcfError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BcfError {}

impl From<ddc_model::Error> for BcfError {
    fn from(e: ddc_model::Error) -> Self {
        BcfError::Model(e)
    }
}

impl From<std::io::Error> for BcfError {
    fn from(e: std::io::Error) -> Self {
        BcfError::Io(e)
    }
}

impl From<zip::result::ZipError> for BcfError {
    fn from(e: zip::result::ZipError) -> Self {
        BcfError::Zip(e.to_string())
    }
}

/// A linked component that matched no element of this model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unmatched {
    pub issue: String,
    /// The IFC guid, or `table:id` of a local reference whose element is gone.
    pub reference: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub version: String,
    pub created: usize,
    pub updated: usize,
    pub elements_matched: usize,
    pub unmatched: Vec<Unmatched>,
    /// Things dropped or tolerated: a missing snapshot file, extra viewpoints.
    pub warnings: Vec<String>,
}

// ---- time ----

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub fn iso(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(86_400));
    let s = ts.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

/// `2026-11-08T12:30:00Z`, with fraction and offset (`+02:00`) or date only; unix seconds.
fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, t),
        None => (s, ""),
    };
    let mut dp = date.splitn(3, '-');
    let y: i64 = dp.next()?.parse().ok()?;
    let m: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(1..=9999).contains(&y) {
        return None;
    }
    let mut secs = 0i64;
    let mut offset = 0i64;
    if !time.is_empty() {
        let (clock, zone) = match time.find(['Z', 'z', '+', '-']) {
            Some(p) => (&time[..p], &time[p..]),
            None => (time, ""),
        };
        let mut tp = clock.splitn(3, ':');
        let h: i64 = tp.next()?.parse().ok()?;
        let mi: i64 = tp.next().unwrap_or("0").parse().ok()?;
        let sec: f64 = tp.next().unwrap_or("0").parse().ok()?;
        if !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0.0..61.0).contains(&sec) {
            return None;
        }
        secs = h * 3600 + mi * 60 + sec as i64;
        if let Some(z) = zone.strip_prefix(['+', '-']) {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let z = z.replace(':', "");
            let oh: i64 = z.get(..2)?.parse().ok()?;
            let om: i64 = z.get(2..4).map_or(Some(0), |x| x.parse().ok())?;
            offset = sign * (oh * 3600 + om * 60);
        }
    }
    Some(days_from_civil(y, m, d) * 86_400 + secs - offset)
}

/// xs:dateTime: `YYYY-MM-DDThh:mm:ss[.f][Z|(+|-)hh:mm]`.
fn is_datetime(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 19 || !s.is_ascii() {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let head = digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b'T'
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19);
    let mut rest = &s[19..];
    if let Some(f) = rest.strip_prefix('.') {
        let n = f.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return false;
        }
        rest = &f[n..];
    }
    let zone = match rest.as_bytes() {
        [] | [b'Z'] => true,
        [sign, h1, h2, b':', m1, m2] => {
            matches!(*sign, b'+' | b'-') && [h1, h2, m1, m2].iter().all(|c| c.is_ascii_digit())
        }
        _ => false,
    };
    head && zone
}

// ---- vocabulary ----

fn status_label(s: &str) -> String {
    match s {
        "open" => "Open".into(),
        "in_progress" => "In Progress".into(),
        "resolved" => "Resolved".into(),
        "closed" => "Closed".into(),
        other => other.to_string(),
    }
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, ' ' | '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect()
}

fn status_from(label: &str) -> String {
    match squash(label).as_str() {
        "open" | "new" | "active" => "open".into(),
        "inprogress" | "assigned" => "in_progress".into(),
        "resolved" | "fixed" => "resolved".into(),
        "closed" | "done" => "closed".into(),
        "" => "open".into(),
        _ => label.trim().to_string(),
    }
}

fn priority_label(s: &str) -> String {
    match s {
        "low" => "Low".into(),
        "normal" => "Normal".into(),
        "high" => "High".into(),
        "critical" => "Critical".into(),
        other => other.to_string(),
    }
}

fn priority_from(label: &str) -> String {
    match squash(label).as_str() {
        "low" | "minor" => "low".into(),
        "normal" | "medium" => "normal".into(),
        "high" | "major" => "high".into(),
        "critical" | "blocker" => "critical".into(),
        "" => "normal".into(),
        _ => label.trim().to_string(),
    }
}

// ---- paths ----

/// An archive name, with `\` read as `/`, that stays inside the archive.
fn safe_name(name: &str) -> Result<String, BcfError> {
    let n = name.replace('\\', "/");
    let bad = n.contains('\0')
        || n.starts_with('/')
        || n.split('/').any(|c| c == ".." || (c.len() == 2 && c.ends_with(':')))
        || n.as_bytes().get(1) == Some(&b':');
    if bad {
        return Err(BcfError::UnsafePath(name.chars().take(120).collect()));
    }
    Ok(n)
}

fn folder_of(path: &str) -> &str {
    path.rfind('/').map_or("", |p| &path[..=p])
}

fn png_or_jpeg(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else {
        None
    }
}

// ---- export ----

pub fn export_file(
    m: &Model,
    ids: Option<&[i64]>,
    v: BcfVersion,
    path: &std::path::Path,
) -> Result<usize, BcfError> {
    let bytes = export_bytes(m, ids, v)?;
    let n = match ids {
        Some(i) => i.len(),
        None => m.topic_list(None)?.len(),
    };
    crate::fsutil::replace_durably_bytes(path, &bytes)
        .map_err(|e| BcfError::Io(std::io::Error::other(e)))?;
    Ok(n)
}

// ---- import ----

fn read_zip(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, BcfError> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes))?;
    if z.len() > MAX_ENTRIES {
        return Err(BcfError::TooLarge("entry count".into()));
    }
    let mut out = BTreeMap::new();
    let mut total = 0u64;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let raw = f.name()?.into_owned();
        let name = safe_name(&raw)?;
        if f.is_dir() || name.ends_with('/') {
            continue;
        }
        if f.size() > MAX_ENTRY_BYTES {
            return Err(BcfError::TooLarge(name));
        }
        let mut buf = Vec::new();
        // the declared size is not trusted: the read itself is capped
        f.by_ref().take(MAX_ENTRY_BYTES + 1).read_to_end(&mut buf)?;
        if buf.len() as u64 > MAX_ENTRY_BYTES {
            return Err(BcfError::TooLarge(name));
        }
        total += buf.len() as u64;
        if total > MAX_TOTAL_BYTES {
            return Err(BcfError::TooLarge("archive".into()));
        }
        out.insert(name, buf);
    }
    Ok(out)
}

fn xml_of(files: &BTreeMap<String, Vec<u8>>, name: &str) -> Result<Option<Node>, BcfError> {
    match files.get(name) {
        Some(b) => {
            let s = std::str::from_utf8(b).map_err(|_| BcfError::Xml("not UTF-8".into()))?;
            Ok(Some(xml::parse(s)?))
        }
        None => Ok(None),
    }
}

fn num(n: &Node, name: &str) -> Option<f64> {
    // num-lint: BCF XML numbers are always dot-decimal
    n.kid_text(name)?.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn point(n: &Node, name: &str) -> Option<[f64; 3]> {
    let p = n.kid(name)?;
    Some([num(p, "X")?, num(p, "Y")?, num(p, "Z")?])
}

fn camera_of(info: &Node) -> Option<Viewpoint> {
    let (cam, ortho) = match (info.kid("PerspectiveCamera"), info.kid("OrthogonalCamera")) {
        (Some(c), _) => (c, false),
        (None, Some(c)) => (c, true),
        _ => return None,
    };
    let clip: Vec<ClipPlane> = info
        .kid("ClippingPlanes")
        .map(|c| {
            c.kids_named("ClippingPlane")
                .filter_map(|p| {
                    Some(ClipPlane {
                        at: point(p, "Location")?,
                        dir: point(p, "Direction")?,
                    })
                })
                .filter(ClipPlane::is_valid)
                .collect()
        })
        .unwrap_or_default();
    let v = Viewpoint {
        eye: point(cam, "CameraViewPoint")?,
        dir: point(cam, "CameraDirection")?,
        up: point(cam, "CameraUpVector")?,
        ortho,
        fov: if ortho { 45.0 } else { num(cam, "FieldOfView").unwrap_or(45.0) },
        scale: if ortho { num(cam, "ViewToWorldScale").unwrap_or(10.0) } else { 0.0 },
        aspect: num(cam, "AspectRatio").filter(|a| *a > 0.0).unwrap_or(0.0),
        clip,
        orbit: None,
    };
    v.is_valid().then_some(v)
}

/// A component id: a child element (the schemas) or an attribute (some older writers).
fn comp_id<'a>(c: &'a Node, name: &str) -> Option<&'a str> {
    c.kid_text(name).or_else(|| c.attr(name)).map(str::trim).filter(|s| !s.is_empty())
}

/// The selected components, and how many named nothing this project can use.
fn components_of(info: &Node, project: &str) -> (Vec<TopicElem>, usize) {
    let mut out = vec![];
    let mut dropped = 0;
    let Some(sel) = info.kid("Components").and_then(|c| c.kid("Selection")) else {
        return (out, 0);
    };
    for c in sel.kids_named("Component") {
        let guid = c.attr("IfcGuid").filter(|g| !g.is_empty()).map(str::to_string);
        let mut e = TopicElem {
            guid,
            ..TopicElem::default()
        };
        let origin = format!("{ORIGIN}:{project}");
        let sys = comp_id(c, "OriginatingSystem");
        let ours = !project.is_empty() && sys == Some(origin.as_str());
        let tool = comp_id(c, "AuthoringToolId").filter(|_| ours || sys == Some(ORIGIN));
        let (local, other) = match tool {
            Some(t) => match t.strip_prefix(GUID_TAG) {
                Some(g) => (None, Some(g)),
                None => match t.split_once(&format!(";{GUID_TAG}")) {
                    Some((a, g)) => (Some(a), Some(g)),
                    None => (Some(t), None),
                },
            },
            None => (None, None),
        };
        if let Some(g) = other.filter(|g| !g.is_empty()) {
            e.guid.get_or_insert_with(|| g.to_string());
        }
        if let Some((t, id)) = local.filter(|_| ours).and_then(|a| a.split_once(':')) {
            if let Ok(id) = id.parse::<i64>() {
                e.table = t.to_string();
                e.id = id;
            }
        }
        if e.guid.is_some() || e.matched() {
            out.push(e);
        } else {
            dropped += 1;
        }
    }
    (out, dropped)
}

fn author_in(a: Option<&str>) -> String {
    match a.unwrap_or("") {
        NO_AUTHOR => String::new(),
        s => s.to_string(),
    }
}

/// Folder of carried documents, written as `documents/<guid>/<name>` (2.1) and
/// `documents/<guid>` with `documents.xml` (3.0).
const DOC_DIR: &str = "documents/";

/// 3.0 `documents.xml`: document guid (lower case) to file name and description.
fn documents_of(files: &BTreeMap<String, Vec<u8>>) -> Result<BTreeMap<String, (String, String)>, BcfError> {
    let mut out = BTreeMap::new();
    if let Some(root) = xml_of(files, "documents.xml")? {
        for d in root.kid("Documents").into_iter().flat_map(|d| d.kids_named("Document")) {
            let (Some(g), Some(n)) = (d.attr("Guid"), d.kid_text("Filename")) else {
                continue;
            };
            let desc = d.kid("Description").map(|x| x.text.clone()).unwrap_or_default();
            out.insert(g.trim().to_ascii_lowercase(), (n.trim().to_string(), desc));
        }
    }
    Ok(out)
}

/// Items of a list written as repeated elements (2.1) or wrapped (3.0): `<Labels>a</Labels>`
/// or `<Labels><Label>a</Label></Labels>`.
fn items(top: &Node, outer: &str, inner: &str) -> Vec<String> {
    let mut out = vec![];
    for n in top.kids_named(outer) {
        if n.kids.is_empty() {
            out.push(n.text.trim().to_string());
        } else {
            out.extend(n.kids_named(inner).map(|k| k.text.trim().to_string()));
        }
    }
    out.retain(|s| !s.is_empty());
    out
}

/// A carried document by its path in the archive; the guid comes from our own layout.
fn doc_file(files: &BTreeMap<String, Vec<u8>>, path: &str, desc_guid: &str) -> Option<TopicDocFile> {
    let data = files.get(path)?;
    if data.len() > TOPIC_DOC_MAX {
        return None;
    }
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    let guid = path
        .strip_prefix(DOC_DIR)
        .and_then(|r| r.split_once('/'))
        .map(|(g, _)| g.to_string())
        .filter(|g| !g.is_empty())
        .unwrap_or_else(|| ddc_model::new_guid(&format!("document|{desc_guid}|{path}")));
    Some(TopicDocFile {
        guid,
        name,
        data: data.clone(),
    })
}

/// The exchange fields of a topic. Words the writer would write anyway (the default type,
/// the standard status and priority words) are not kept, so our own files read back as written.
fn exchange_of(
    files: &BTreeMap<String, Vec<u8>>,
    docs3: &BTreeMap<String, (String, String)>,
    top: &Node,
    t: &Topic,
    dir: &str,
    warn: &mut Vec<String>,
) -> Result<TopicBcf, BcfError> {
    let mut b = TopicBcf {
        topic_type: top.attr("TopicType").unwrap_or("").trim().to_string(),
        labels: items(top, "Labels", "Label"),
        stage: top.kid_text("Stage").unwrap_or("").trim().to_string(),
        links: [items(top, "ReferenceLink", ""), items(top, "ReferenceLinks", "ReferenceLink")].concat(),
        status_word: top.attr("TopicStatus").unwrap_or("").trim().to_string(),
        priority_word: top.kid_text("Priority").unwrap_or("").trim().to_string(),
        ..TopicBcf::default()
    };
    if b.topic_type == write::TOPIC_TYPE {
        b.topic_type.clear();
    }
    if b.status_word == status_label(&t.status) {
        b.status_word.clear();
    }
    if b.priority_word == priority_label(&t.priority) || b.priority_word.is_empty() {
        b.priority_word.clear();
    }
    if let Some(d) = top.kid_text("DueDate").map(str::trim).filter(|d| !d.is_empty()) {
        if is_datetime(d) {
            b.due = d.to_string();
        } else {
            warn.push(format!("{}: due date {d:?} is not a date and time", t.title));
        }
    }
    // 2.1: repeated DocumentReference; 3.0: DocumentReferences/DocumentReference
    let refs: Vec<&Node> = top
        .kids_named("DocumentReference")
        .chain(top.kids_named("DocumentReferences").flat_map(|d| d.kids_named("DocumentReference")))
        .collect();
    for (i, r) in refs.into_iter().enumerate() {
        let guid = r
            .attr("Guid")
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map_or_else(|| ddc_model::new_guid(&format!("docref|{}|{i}", t.guid)), str::to_string);
        let description = r.kid("Description").map(|x| x.text.clone()).unwrap_or_default();
        let mut d = TopicDoc {
            guid,
            description,
            ..TopicDoc::default()
        };
        if let Some(u) = r.kid_text("Url").map(str::trim).filter(|u| !u.is_empty()) {
            d.url = u.to_string();
        } else if let Some(g) = r.kid_text("DocumentGuid").map(str::trim).filter(|g| !g.is_empty()) {
            let key = g.to_ascii_lowercase();
            let path = format!("{DOC_DIR}{key}");
            match (docs3.get(&key), files.get(&path)) {
                (Some((name, _)), Some(data)) if data.len() <= TOPIC_DOC_MAX => {
                    d.file = Some(TopicDocFile {
                        guid: g.to_string(),
                        name: name.clone(),
                        data: data.clone(),
                    });
                }
                _ => {
                    warn.push(format!("{}: document {g} is missing or too large", t.title));
                    continue;
                }
            }
        } else if let Some(p) = r.kid_text("ReferencedDocument").map(str::trim).filter(|p| !p.is_empty()) {
            // the attribute defaults to false, but writers often leave it off for addresses
            let external = matches!(r.attr("isExternal"), Some("true" | "1")) || p.contains("://");
            if external {
                d.url = p.to_string();
            } else {
                // a path that leaves the archive (or names a disk) is not read, only reported
                let found = resolve(dir, p).ok().and_then(|path| doc_file(files, &path, &d.guid));
                match found {
                    Some(f) => d.file = Some(f),
                    None => {
                        warn.push(format!("{}: document {p} is missing or too large", t.title));
                        continue;
                    }
                }
            }
        } else {
            continue;
        }
        b.docs.push(d);
    }
    Ok(b)
}

/// `rel` against folder `dir` of the archive, `..` steps allowed while they stay inside.
fn resolve(dir: &str, rel: &str) -> Result<String, BcfError> {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let rel_fwd = rel.replace('\\', "/");
    for c in rel_fwd.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(BcfError::UnsafePath(rel.chars().take(120).collect()));
                }
            }
            c => parts.push(c),
        }
    }
    safe_name(&parts.join("/"))
}

fn parse_topic(
    files: &BTreeMap<String, Vec<u8>>,
    docs3: &BTreeMap<String, (String, String)>,
    markup_path: &str,
    project: &str,
    warn: &mut Vec<String>,
) -> Result<Option<Topic>, BcfError> {
    let Some(root) = xml_of(files, markup_path)? else {
        return Ok(None);
    };
    let Some(top) = root.kid("Topic") else {
        warn.push(format!("{markup_path}: no Topic element"));
        return Ok(None);
    };
    let guid = top.attr("Guid").unwrap_or("").trim().to_string();
    let title = top.kid_text("Title").unwrap_or("").to_string();
    if guid.is_empty() || title.is_empty() {
        warn.push(format!("{markup_path}: topic without guid or title skipped"));
        return Ok(None);
    }
    let mut t = Topic::new(&title);
    t.guid = guid;
    t.status = status_from(top.attr("TopicStatus").unwrap_or(""));
    t.priority = priority_from(top.kid_text("Priority").unwrap_or(""));
    t.assignee = top.kid_text("AssignedTo").unwrap_or("").to_string();
    t.description = top.kid("Description").map(|d| d.text.clone()).unwrap_or_default();
    t.author = author_in(top.kid_text("CreationAuthor"));
    t.created = top.kid_text("CreationDate").and_then(parse_iso).unwrap_or(0);
    t.modified = top.kid_text("ModifiedDate").and_then(parse_iso).unwrap_or(t.created);
    t.source = "bcf".to_string();
    t.bcf = exchange_of(files, docs3, top, &t, folder_of(markup_path), warn)?;

    // 2.x keeps comments and viewpoints beside the topic, 3.0 inside it
    let mut all = vec![];
    root.descendants(&mut all);
    for c in all.iter().filter(|n| n.name == "Comment" && n.attr("Guid").is_some()) {
        let text = c.kid("Comment").map(|x| x.text.clone()).unwrap_or_default();
        if text.trim().is_empty() {
            continue;
        }
        t.comments.push(TopicComment {
            guid: c.attr("Guid").unwrap_or("").to_string(),
            ts: c.kid_text("Date").and_then(parse_iso).unwrap_or(0),
            author: author_in(c.kid_text("Author")),
            text,
        });
    }
    let dir = folder_of(markup_path);
    let mut seen = 0;
    for n in all.iter().filter(|n| matches!(n.name.as_str(), "Viewpoints" | "ViewPoint")) {
        let vfile = n.kid_text("Viewpoint").filter(|s| !s.is_empty());
        let sfile = n.kid_text("Snapshot").filter(|s| !s.is_empty());
        if vfile.is_none() && sfile.is_none() {
            continue;
        }
        seen += 1;
        if let Some(vf) = vfile {
            safe_name(vf)?;
            let p = safe_name(&format!("{dir}{vf}"))?;
            match xml_of(files, &p)? {
                Some(info) => {
                    if t.viewpoint.is_none() {
                        t.viewpoint = camera_of(&info);
                    }
                    let (found, dropped) = components_of(&info, project);
                    for e in found {
                        if !t.elements.contains(&e) {
                            t.elements.push(e);
                        }
                    }
                    if dropped > 0 {
                        warn.push(format!(
                            "{}: {dropped} component(s) without an IFC guid",
                            t.title
                        ));
                    }
                }
                None => warn.push(format!("{p}: viewpoint file missing")),
            }
        }
        if let Some(sf) = sfile.filter(|_| t.snapshot.is_none()) {
            safe_name(sf)?;
            let p = safe_name(&format!("{dir}{sf}"))?;
            match files.get(&p) {
                Some(b) if png_or_jpeg(b).is_some() && b.len() <= SNAPSHOT_MAX => {
                    t.snapshot = Some(b.clone());
                }
                Some(_) => warn.push(format!("{p}: snapshot is not a usable image")),
                None => warn.push(format!("{p}: snapshot file missing")),
            }
        }
    }
    if seen > 1 {
        warn.push(format!("{}: only the first camera is kept", t.title));
    }
    Ok(Some(t))
}

/// Reads every topic of a BCF zip. `project` is the tag of this project (`topic_project_tag`):
/// components written by this editor for the same project match by table and id.
pub fn parse(bytes: &[u8], project: &str) -> Result<(String, Vec<Topic>, Vec<String>), BcfError> {
    let files = read_zip(bytes)?;
    let ver = xml_of(&files, "bcf.version")?
        .ok_or_else(|| BcfError::Version("bcf.version is missing".into()))?;
    let version = ver.attr("VersionId").unwrap_or("").trim().to_string();
    if !matches!(version.as_str(), "2.0" | "2.1" | "3.0") {
        return Err(BcfError::Version(version));
    }
    let mut warn = vec![];
    let mut topics = vec![];
    let docs3 = documents_of(&files)?;
    let paths: Vec<&String> = files
        .keys()
        .filter(|k| !k.starts_with(DOC_DIR))
        .filter(|k| k.rsplit('/').next().is_some_and(|f| f.eq_ignore_ascii_case("markup.bcf")))
        .collect();
    if paths.len() > MAX_TOPICS {
        return Err(BcfError::TooLarge("topic count".into()));
    }
    for p in paths {
        if let Some(t) = parse_topic(&files, &docs3, p, project, &mut warn)? {
            if topics.iter().any(|x: &Topic| x.guid == t.guid) {
                warn.push(format!("{}: duplicate guid skipped", t.title));
            } else {
                topics.push(t);
            }
        }
    }
    Ok((version, topics, warn))
}

/// Imports a BCF zip into the project: new guids are added, known ones replaced. Linked
/// components are matched to elements by IFC guid; the rest are kept on the issue and listed.
pub fn import_bytes(m: &Model, bytes: &[u8]) -> Result<ImportReport, BcfError> {
    let (version, topics, warnings) = parse(bytes, &m.topic_project_tag())?;
    let mut rep = ImportReport {
        version,
        warnings,
        ..ImportReport::default()
    };
    for mut t in topics {
        m.topic_resolve(&mut t.elements)?;
        let mut kept = vec![];
        for e in std::mem::take(&mut t.elements) {
            if e.matched() && m.topic_elem_alive(&e.table, e.id) {
                rep.elements_matched += 1;
                kept.push(e);
            } else {
                rep.unmatched.push(Unmatched {
                    issue: t.title.clone(),
                    reference: e
                        .guid
                        .clone()
                        .unwrap_or_else(|| format!("{}:{}", e.table, e.id)),
                });
                if let Some(g) = e.guid {
                    kept.push(TopicElem::external(&g));
                }
            }
        }
        t.elements = kept;
        match m.topic_import(&t)? {
            TopicImport::Created(_) => rep.created += 1,
            TopicImport::Updated(_) => rep.updated += 1,
        }
    }
    Ok(rep)
}

pub fn import_file(m: &Model, path: &std::path::Path) -> Result<ImportReport, BcfError> {
    let len = std::fs::metadata(path)?.len();
    if len > MAX_TOTAL_BYTES {
        return Err(BcfError::TooLarge(path.display().to_string()));
    }
    import_bytes(m, &std::fs::read(path)?)
}
