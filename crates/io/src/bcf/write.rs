//! BCF writer. Follows the published 2.1 and 3.0 schemas element by element (`schema` checks
//! every file in the tests): component ids as child elements, the 3.0 aspect ratio and the
//! camera every 3.0 viewpoint needs, lower-case 3.0 guids, the 2.1 field-of-view range.

use std::collections::BTreeSet;
use std::io::{Cursor, Write};

use ddc_model::{
    Model, Orbit, Topic, TopicDocFile, TopicElem, Viewpoint, TOPIC_PRIORITIES, TOPIC_STATUSES,
};

use super::xml::esc;
use super::{
    iso, png_or_jpeg, priority_from, priority_label, status_from, status_label, BcfError,
    BcfVersion, DOC_DIR, GUID_TAG, NO_AUTHOR, ORIGIN,
};

/// Aspect written when the view did not record one; 3.0 needs it.
pub(super) const DEFAULT_ASPECT: f64 = 16.0 / 9.0;
/// BCF 2.1 limits the perspective field of view to this range, in degrees.
pub(super) const FOV_21: (f64, f64) = (45.0, 60.0);
/// Distance to the pivot of a camera that has no orbit, for the 2.1 field-of-view fit.
const PIVOT_GUESS: f64 = 10.0;
/// Topic type written for an issue that has none of its own.
pub(super) const TOPIC_TYPE: &str = "Issue";

fn tag(name: &str, text: &str) -> String {
    format!("<{name}>{}</{name}>", esc(text))
}

fn vec3(name: &str, v: [f64; 3]) -> String {
    format!(
        "<{name}><X>{}</X><Y>{}</Y><Z>{}</Z></{name}>",
        v[0], v[1], v[2]
    )
}

/// The 22-character IFC GlobalId form the schemas require of `IfcGuid`.
pub(super) fn is_ifc_guid(g: &str) -> bool {
    g.len() == 22 && g.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
}

fn guid_out(g: &str, v: BcfVersion) -> String {
    match v {
        BcfVersion::V30 => g.to_ascii_lowercase(),
        BcfVersion::V21 => g.to_string(),
    }
}

/// A document guid as the file needs it: kept when it is one, else derived from it.
fn doc_guid(g: &str, v: BcfVersion) -> String {
    let b = g.as_bytes();
    let ok = b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        });
    if ok {
        guid_out(g, v)
    } else {
        ddc_model::new_guid(&format!("document|{g}")).to_ascii_lowercase()
    }
}

/// 3.0 wants a non-blank author; an issue without one is written with a placeholder that the
/// reader turns back into an empty author.
fn author_out(a: &str, v: BcfVersion) -> String {
    if v == BcfVersion::V30 && a.trim().is_empty() {
        NO_AUTHOR.to_string()
    } else {
        a.to_string()
    }
}

/// A linked element. A guid that is not an IFC GlobalId travels in `AuthoringToolId` after
/// `guid:`; a local element of this project as `table:id` under `ddc-editor:<project>`.
fn component(e: &TopicElem, project: &str) -> Option<String> {
    let guid = e.guid.as_deref().filter(|g| !g.is_empty());
    let ifc = guid.filter(|g| is_ifc_guid(g));
    let other = guid.filter(|g| !is_ifc_guid(g));
    let local = e.matched() && !project.is_empty();
    let mut tool = if local {
        format!("{}:{}", e.table, e.id)
    } else {
        String::new()
    };
    if let Some(g) = other {
        if !tool.is_empty() {
            tool.push(';');
        }
        tool += GUID_TAG;
        tool += g;
    }
    if ifc.is_none() && tool.is_empty() {
        return None;
    }
    let mut s = String::from("<Component");
    if let Some(g) = ifc {
        s += &format!(" IfcGuid=\"{}\"", esc(g));
    }
    s.push('>');
    if !tool.is_empty() {
        let origin = if local {
            format!("{ORIGIN}:{project}")
        } else {
            ORIGIN.to_string()
        };
        s += &tag("OriginatingSystem", &origin);
        s += &tag("AuthoringToolId", &tool);
    }
    Some(s + "</Component>")
}

/// A 2.1 perspective camera with its field of view inside 45..60 degrees: the eye moves along
/// the view so the frame at the pivot keeps its size.
pub(super) fn fit_fov_21(v: &Viewpoint) -> Viewpoint {
    let f = v.fov.clamp(FOV_21.0, FOV_21.1);
    if v.ortho || f == v.fov {
        return v.clone();
    }
    if (f - v.fov).abs() <= 1e-9 {
        // 45 degrees through radians and back: a rounding, not a wider lens
        return Viewpoint { fov: f, ..v.clone() };
    }
    let d = v.orbit.map_or(PIVOT_GUESS, |o| o.distance);
    let d2 = d * (v.fov.to_radians() * 0.5).tan() / (f.to_radians() * 0.5).tan();
    let n = v.dir.iter().map(|x| x * x).sum::<f64>().sqrt();
    let mut out = v.clone();
    for i in 0..3 {
        out.eye[i] += v.dir[i] / n * (d - d2);
    }
    out.fov = f;
    out.orbit = None;
    out
}

fn camera_xml(v: &Viewpoint, ver: BcfVersion) -> String {
    let v = if ver == BcfVersion::V21 {
        fit_fov_21(v)
    } else {
        v.clone()
    };
    let cam = if v.ortho { "OrthogonalCamera" } else { "PerspectiveCamera" };
    let mut s = format!("<{cam}>");
    s += &vec3("CameraViewPoint", v.eye);
    s += &vec3("CameraDirection", v.dir);
    s += &vec3("CameraUpVector", v.up);
    if v.ortho {
        s += &format!("<ViewToWorldScale>{}</ViewToWorldScale>", v.scale);
    } else {
        s += &format!("<FieldOfView>{}</FieldOfView>", v.fov);
    }
    if ver == BcfVersion::V30 {
        let a = if v.aspect > 0.0 { v.aspect } else { DEFAULT_ASPECT };
        s += &format!("<AspectRatio>{a}</AspectRatio>");
    }
    s + &format!("</{cam}>\n")
}

fn viewpoint_xml(
    t: &Topic,
    cam: Option<&Viewpoint>,
    vp_guid: &str,
    project: &str,
    ver: BcfVersion,
) -> String {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s += &format!("<VisualizationInfo Guid=\"{vp_guid}\">\n<Components>\n");
    let comps: Vec<String> = t.elements.iter().filter_map(|e| component(e, project)).collect();
    if !comps.is_empty() {
        s += "<Selection>\n";
        for c in comps {
            s += &c;
            s.push('\n');
        }
        s += "</Selection>\n";
    }
    s += "<Visibility DefaultVisibility=\"true\"/>\n</Components>\n";
    if let Some(v) = cam {
        s += &camera_xml(v, ver);
        if !v.clip.is_empty() {
            s += "<ClippingPlanes>\n";
            for p in &v.clip {
                s += "<ClippingPlane>";
                s += &vec3("Location", p.at);
                s += &vec3("Direction", p.dir);
                s += "</ClippingPlane>\n";
            }
            s += "</ClippingPlanes>\n";
        }
    }
    s + "</VisualizationInfo>\n"
}

/// The file's own word while it still means the stored value, else the standard word.
fn word_out(word: &str, stored: &str, from: fn(&str) -> String, label: fn(&str) -> String) -> String {
    if !word.trim().is_empty() && from(word) == stored {
        word.to_string()
    } else {
        label(stored)
    }
}

/// A list written as repeated elements (2.1) or wrapped (3.0).
fn list_out(v: BcfVersion, outer: &str, inner: &str, items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    match v {
        BcfVersion::V21 => items.iter().map(|i| tag(outer, i)).collect(),
        BcfVersion::V30 => {
            let body: String = items.iter().map(|i| tag(inner, i)).collect();
            format!("<{outer}>{body}</{outer}>")
        }
    }
}

/// Path of a carried document in the archive.
pub(super) fn doc_path(f: &TopicDocFile, v: BcfVersion) -> String {
    match v {
        BcfVersion::V21 => format!("{DOC_DIR}{}/{}", doc_guid(&f.guid, v), doc_name(&f.name)),
        BcfVersion::V30 => format!("{DOC_DIR}{}", doc_guid(&f.guid, v)),
    }
}

/// A file name that stays one archive entry: no folders, no control characters.
fn doc_name(n: &str) -> String {
    let s: String = n
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == ':' || c.is_control() { '_' } else { c })
        .collect();
    match s.trim_matches('.') {
        "" => "document".into(),
        _ => s,
    }
}

fn doc_refs_xml(t: &Topic, v: BcfVersion) -> String {
    let mut s = String::new();
    for d in &t.bcf.docs {
        let desc = if d.description.trim().is_empty() { String::new() } else { tag("Description", &d.description) };
        let g = esc(&doc_guid(&d.guid, v));
        s += &match (v, &d.file) {
            (BcfVersion::V21, Some(f)) => format!(
                "<DocumentReference Guid=\"{g}\" isExternal=\"false\">{}{desc}</DocumentReference>",
                tag("ReferencedDocument", &format!("../{}", doc_path(f, v)))
            ),
            (BcfVersion::V21, None) => format!(
                "<DocumentReference Guid=\"{g}\" isExternal=\"true\">{}{desc}</DocumentReference>",
                tag("ReferencedDocument", &d.url)
            ),
            (BcfVersion::V30, Some(f)) => format!(
                "<DocumentReference Guid=\"{g}\">{}{desc}</DocumentReference>",
                tag("DocumentGuid", &doc_guid(&f.guid, v))
            ),
            (BcfVersion::V30, None) => {
                format!("<DocumentReference Guid=\"{g}\">{}{desc}</DocumentReference>", tag("Url", &d.url))
            }
        };
    }
    match v {
        BcfVersion::V30 if !s.is_empty() => format!("<DocumentReferences>{s}</DocumentReferences>"),
        _ => s,
    }
}

/// 3.0 `documents.xml` of the carried documents; `None` when there are none.
fn documents_xml(topics: &[Topic]) -> Option<String> {
    let mut s = String::new();
    let mut seen = BTreeSet::new();
    for f in topics.iter().flat_map(|t| t.bcf.docs.iter().filter_map(|d| d.file.as_ref())) {
        let g = doc_guid(&f.guid, BcfVersion::V30);
        if seen.insert(g.clone()) {
            s += &format!("<Document Guid=\"{}\">{}</Document>\n", esc(&g), tag("Filename", &doc_name(&f.name)));
        }
    }
    (!s.is_empty()).then(|| {
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DocumentInfo>\n<Documents>\n{s}</Documents>\n</DocumentInfo>\n")
    })
}

fn markup_xml(t: &Topic, v: BcfVersion, vp_ref: Option<(&str, Option<&str>, Option<String>)>) -> String {
    let b = &t.bcf;
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Markup>\n");
    let kind = if b.topic_type.trim().is_empty() { TOPIC_TYPE } else { b.topic_type.as_str() };
    s += &format!(
        "<Topic Guid=\"{}\" TopicType=\"{}\" TopicStatus=\"{}\">\n",
        esc(&guid_out(&t.guid, v)),
        esc(kind),
        esc(&word_out(&b.status_word, &t.status, status_from, status_label))
    );
    s += &list_out(v, if v == BcfVersion::V30 { "ReferenceLinks" } else { "ReferenceLink" }, "ReferenceLink", &b.links);
    s += &tag("Title", &t.title);
    s += &tag("Priority", &word_out(&b.priority_word, &t.priority, priority_from, priority_label));
    s += &list_out(v, "Labels", "Label", &b.labels);
    s += &tag("CreationDate", &iso(t.created));
    s += &tag("CreationAuthor", &author_out(&t.author, v));
    s += &tag("ModifiedDate", &iso(t.modified));
    if !b.due.is_empty() {
        s += &tag("DueDate", &b.due);
    }
    if !t.assignee.trim().is_empty() {
        s += &tag("AssignedTo", &t.assignee);
    }
    if !b.stage.trim().is_empty() {
        s += &tag("Stage", &b.stage);
    }
    if !t.description.trim().is_empty() {
        s += &tag("Description", &t.description);
    }
    s += &doc_refs_xml(t, v);
    let comments = comments_xml(t, v);
    let views = match vp_ref {
        Some((vp_guid, vp_file, snap)) => {
            let item = if v == BcfVersion::V30 { "ViewPoint" } else { "Viewpoints" };
            let mut x = format!("<{item} Guid=\"{vp_guid}\">");
            if let Some(f) = vp_file {
                x += &tag("Viewpoint", f);
            }
            if let Some(n) = snap {
                x += &tag("Snapshot", &n);
            }
            x += &format!("</{item}>\n");
            if v == BcfVersion::V30 {
                format!("<Viewpoints>\n{x}</Viewpoints>\n")
            } else {
                x
            }
        }
        None => String::new(),
    };
    match v {
        BcfVersion::V21 => {
            s += "\n</Topic>\n";
            s += &comments;
            s += &views;
        }
        BcfVersion::V30 => {
            if !comments.is_empty() {
                s += &format!("\n<Comments>\n{comments}</Comments>");
            }
            s += &views;
            s += "</Topic>\n";
        }
    }
    s + "</Markup>\n"
}

fn comments_xml(t: &Topic, v: BcfVersion) -> String {
    let mut s = String::new();
    for c in &t.comments {
        s += &format!(
            "<Comment Guid=\"{}\">{}{}{}</Comment>\n",
            esc(&guid_out(&c.guid, v)),
            tag("Date", &iso(c.ts)),
            tag("Author", &author_out(&c.author, v)),
            tag("Comment", &c.text)
        );
    }
    s
}

/// 3.0 `extensions.xml`: the vocabulary the topics use, so a reader can offer it.
fn extensions_xml(topics: &[Topic]) -> String {
    let mut statuses: BTreeSet<String> = TOPIC_STATUSES.iter().map(|s| status_label(s)).collect();
    let mut prios: BTreeSet<String> = TOPIC_PRIORITIES.iter().map(|s| priority_label(s)).collect();
    let mut users = BTreeSet::new();
    let mut kinds = BTreeSet::from([TOPIC_TYPE.to_string()]);
    let (mut labels, mut stages) = (BTreeSet::new(), BTreeSet::new());
    for t in topics {
        let b = &t.bcf;
        statuses.insert(word_out(&b.status_word, &t.status, status_from, status_label));
        prios.insert(word_out(&b.priority_word, &t.priority, priority_from, priority_label));
        kinds.insert(b.topic_type.clone());
        labels.extend(b.labels.iter().cloned());
        stages.insert(b.stage.clone());
        for u in [&t.author, &t.assignee].into_iter().chain(t.comments.iter().map(|c| &c.author)) {
            if !u.trim().is_empty() {
                users.insert(u.clone());
            }
        }
    }
    let list = |outer: &str, inner: &str, items: &BTreeSet<String>| -> String {
        let items: Vec<&String> = items.iter().filter(|i| !i.trim().is_empty()).collect();
        if items.is_empty() {
            return String::new();
        }
        let body: String = items.into_iter().map(|i| tag(inner, i)).collect();
        format!("<{outer}>{body}</{outer}>\n")
    };
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Extensions>\n");
    s += &list("TopicTypes", "TopicType", &kinds);
    s += &list("TopicStatuses", "TopicStatus", &statuses);
    s += &list("Priorities", "Priority", &prios);
    s += &list("TopicLabels", "TopicLabel", &labels);
    s += &list("Users", "User", &users);
    s += &list("Stages", "Stage", &stages);
    s + "</Extensions>\n"
}

fn folder_name(guid: &str, index: usize) -> String {
    let f: String = guid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if f.is_empty() {
        format!("topic-{index}")
    } else {
        f
    }
}

/// The camera a 3.0 viewpoint gets for an issue that has elements but no saved view: one that
/// frames them, else a general view of the origin.
fn stand_in_camera(m: &Model, t: &Topic) -> Result<Viewpoint, BcfError> {
    if let Some(v) = m.topic_frame_view(&t.elements)? {
        return Ok(v);
    }
    Ok(Viewpoint::from_orbit(&Orbit {
        target: [0.0; 3],
        yaw: -0.75 * std::f64::consts::PI,
        pitch: 0.6,
        distance: 50.0,
        fov_y: 45f64.to_radians(),
        ortho: false,
    }))
}

/// Writes the issues (`ids`, or all when `None`) as a BCF zip.
pub fn export_bytes(m: &Model, ids: Option<&[i64]>, v: BcfVersion) -> Result<Vec<u8>, BcfError> {
    let topics: Vec<Topic> = match ids {
        Some(ids) => ids.iter().map(|i| m.topic_get(*i)).collect::<Result<_, _>>()?,
        None => m.topic_all()?,
    };
    let project = m.topic_project_tag();
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opt = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut put = |name: &str, data: &[u8]| -> Result<(), BcfError> {
        w.start_file(name, opt)?;
        w.write_all(data)?;
        Ok(())
    };
    // 3.0 dropped `DetailedVersion`
    let detail = match v {
        BcfVersion::V21 => format!("<DetailedVersion>{}</DetailedVersion>", v.id()),
        BcfVersion::V30 => String::new(),
    };
    let ver = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Version VersionId=\"{}\">{detail}</Version>\n",
        v.id()
    );
    put("bcf.version", ver.as_bytes())?;
    if v == BcfVersion::V30 {
        put("extensions.xml", extensions_xml(&topics).as_bytes())?;
        if let Some(x) = documents_xml(&topics) {
            put("documents.xml", x.as_bytes())?;
        }
    }
    let mut docs = BTreeSet::new();
    for f in topics.iter().flat_map(|t| t.bcf.docs.iter().filter_map(|d| d.file.as_ref())) {
        let path = doc_path(f, v);
        if docs.insert(path.clone()) {
            put(&path, &f.data)?;
        }
    }
    let mut used = BTreeSet::new();
    for (i, t) in topics.iter().enumerate() {
        let mut dir = folder_name(&guid_out(&t.guid, v), i);
        while !used.insert(dir.clone()) {
            dir.push('x');
        }
        let has_view = t.viewpoint.is_some() || !t.elements.is_empty();
        let cam = match (&t.viewpoint, v) {
            (Some(c), _) => Some(c.clone()),
            (None, BcfVersion::V30) if has_view => Some(stand_in_camera(m, t)?),
            (None, _) => None,
        };
        let snap_ext = t.snapshot.as_deref().and_then(png_or_jpeg);
        let vp_guid = ddc_model::new_guid(&format!("viewpoint|{}", t.guid));
        let vp_file = "viewpoint.bcfv";
        let snap_name = snap_ext.map(|e| format!("snapshot.{e}"));
        let vp_ref = (has_view || snap_name.is_some()).then(|| {
            (vp_guid.as_str(), has_view.then_some(vp_file), snap_name.clone())
        });
        put(&format!("{dir}/markup.bcf"), markup_xml(t, v, vp_ref).as_bytes())?;
        if has_view {
            let x = viewpoint_xml(t, cam.as_ref(), &vp_guid, &project, v);
            put(&format!("{dir}/{vp_file}"), x.as_bytes())?;
        }
        if let (Some(n), Some(b)) = (&snap_name, &t.snapshot) {
            put(&format!("{dir}/{n}"), b)?;
        }
    }
    Ok(w.finish()?.into_inner())
}
