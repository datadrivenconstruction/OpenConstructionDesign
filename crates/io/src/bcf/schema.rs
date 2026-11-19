//! Test-only structural check of BCF files against the published 2.1 and 3.0 schemas: element
//! order and occurrence, required and allowed attributes, the guid and IFC guid patterns, dates,
//! numbers and their ranges (2.1 field of view 45..60, 3.0 0..180 and a positive aspect ratio),
//! non-blank strings of 3.0, and the camera every 3.0 viewpoint must have. Our own encoding of
//! the rules, written from the schema text; strict: anything undeclared is an error.

use super::xml::{self, Node};
use super::BcfVersion;

#[derive(Clone, Copy, Debug)]
enum Leaf {
    Str,
    NonBlank,
    Double,
    /// Inclusive (2.1) or exclusive (3.0) range.
    Range(f64, f64, bool),
    Positive,
    Int,
    Date,
    Guid,
}

#[derive(Clone, Copy, Debug)]
enum Attr {
    Str,
    NonBlank,
    Guid,
    IfcGuid,
    Bool,
}

/// An element type: leaf text, or a sequence of children.
struct Ty {
    name: &'static str,
    attrs: Vec<(&'static str, Attr, bool)>,
    /// (element, type, min, max); `MANY` is unbounded.
    kids: Vec<(&'static str, &'static str, u32, u32)>,
    /// Exactly one of these children must be present.
    choice: Vec<&'static str>,
    leaf: Option<Leaf>,
}

const MANY: u32 = u32::MAX;

fn leaf(name: &'static str, l: Leaf) -> Ty {
    Ty { name, attrs: vec![], kids: vec![], choice: vec![], leaf: Some(l) }
}

fn node(
    name: &'static str,
    attrs: Vec<(&'static str, Attr, bool)>,
    kids: Vec<(&'static str, &'static str, u32, u32)>,
) -> Ty {
    Ty { name, attrs, kids, choice: vec![], leaf: None }
}

/// Which file of the zip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum File {
    Version,
    Markup,
    Visinfo,
    Extensions,
    /// 3.0 `documents.xml`.
    Documents,
}

fn common(v: BcfVersion) -> Vec<Ty> {
    let v3 = v == BcfVersion::V30;
    let nb = if v3 { Leaf::NonBlank } else { Leaf::Str };
    let fov = if v3 { Leaf::Range(0.0, 180.0, false) } else { Leaf::Range(45.0, 60.0, true) };
    let mut t = vec![
        leaf("str", Leaf::Str),
        leaf("nb", nb),
        leaf("double", Leaf::Double),
        leaf("fov", fov),
        leaf("pos", Leaf::Positive),
        leaf("int", Leaf::Int),
        leaf("date", Leaf::Date),
        leaf("guid", Leaf::Guid),
        node("Point", vec![], vec![("X", "double", 1, 1), ("Y", "double", 1, 1), ("Z", "double", 1, 1)]),
        node("Ref", vec![("Guid", Attr::Guid, true)], vec![]),
        node(
            "File",
            vec![
                ("IfcProject", Attr::IfcGuid, false),
                ("IfcSpatialStructureElement", Attr::IfcGuid, false),
                (if v3 { "IsExternal" } else { "isExternal" }, Attr::Bool, false),
            ],
            vec![("Filename", "str", 0, 1), ("Date", "date", 0, 1), ("Reference", "str", 0, 1)],
        ),
        node(
            "BimSnippet",
            vec![("SnippetType", Attr::Str, true), ("isExternal", Attr::Bool, false)],
            vec![("Reference", "str", 1, 1), ("ReferenceSchema", "str", 1, 1)],
        ),
        node(
            "ViewPoint",
            vec![("Guid", Attr::Guid, true)],
            vec![("Viewpoint", "str", 0, 1), ("Snapshot", "str", 0, 1), ("Index", "int", 0, 1)],
        ),
        node(
            "Hints",
            vec![
                ("SpacesVisible", Attr::Bool, false),
                ("SpaceBoundariesVisible", Attr::Bool, false),
                ("OpeningsVisible", Attr::Bool, false),
            ],
            vec![],
        ),
        node(
            "Comp",
            vec![("IfcGuid", Attr::IfcGuid, false)],
            vec![("OriginatingSystem", "nb", 0, 1), ("AuthoringToolId", "nb", 0, 1)],
        ),
        node("Sel", vec![], vec![("Component", "Comp", 0, MANY)]),
        node("Color", vec![("Color", Attr::Str, true)], vec![("Component", "Comp", 1, MANY)]),
        node("Coloring", vec![], vec![("Color", "Color", 0, MANY)]),
        node("Line", vec![], vec![("StartPoint", "Point", 1, 1), ("EndPoint", "Point", 1, 1)]),
        node("Lines", vec![], vec![("Line", "Line", 0, MANY)]),
        node("Clip", vec![], vec![("Location", "Point", 1, 1), ("Direction", "Point", 1, 1)]),
        node("Clips", vec![], vec![("ClippingPlane", "Clip", 0, MANY)]),
    ];
    let cam = |scale: (&'static str, &'static str)| {
        let mut k = vec![
            ("CameraViewPoint", "Point", 1, 1),
            ("CameraDirection", "Point", 1, 1),
            ("CameraUpVector", "Point", 1, 1),
            (scale.0, scale.1, 1, 1),
        ];
        if v3 {
            k.push(("AspectRatio", "pos", 1, 1));
        }
        k
    };
    t.push(node("Ortho", vec![], cam(("ViewToWorldScale", "double"))));
    t.push(node("Persp", vec![], cam(("FieldOfView", "fov"))));
    t
}

fn v21() -> Vec<Ty> {
    let mut t = common(BcfVersion::V21);
    t.extend([
        node("Version", vec![("VersionId", Attr::Str, true)], vec![("DetailedVersion", "str", 0, 1)]),
        node("Header", vec![], vec![("File", "File", 1, MANY)]),
        node(
            "Markup",
            vec![],
            vec![
                ("Header", "Header", 0, 1),
                ("Topic", "Topic", 1, 1),
                ("Comment", "Comment", 0, MANY),
                ("Viewpoints", "ViewPoint", 0, MANY),
            ],
        ),
        node(
            "Topic",
            vec![
                ("Guid", Attr::Guid, true),
                ("TopicType", Attr::Str, false),
                ("TopicStatus", Attr::Str, false),
            ],
            vec![
                ("ReferenceLink", "str", 0, MANY),
                ("Title", "str", 1, 1),
                ("Priority", "str", 0, 1),
                ("Index", "int", 0, 1),
                ("Labels", "str", 0, MANY),
                ("CreationDate", "date", 1, 1),
                ("CreationAuthor", "str", 1, 1),
                ("ModifiedDate", "date", 0, 1),
                ("ModifiedAuthor", "str", 0, 1),
                ("DueDate", "date", 0, 1),
                ("AssignedTo", "str", 0, 1),
                ("Stage", "str", 0, 1),
                ("Description", "str", 0, 1),
                ("BimSnippet", "BimSnippet", 0, 1),
                ("DocumentReference", "DocRef", 0, MANY),
                ("RelatedTopic", "Ref", 0, MANY),
            ],
        ),
        node(
            "DocRef",
            vec![("Guid", Attr::Guid, false), ("isExternal", Attr::Bool, false)],
            vec![("ReferencedDocument", "str", 0, 1), ("Description", "str", 0, 1)],
        ),
        node(
            "Comment",
            vec![("Guid", Attr::Guid, true)],
            vec![
                ("Date", "date", 1, 1),
                ("Author", "str", 1, 1),
                ("Comment", "str", 1, 1),
                ("Viewpoint", "Ref", 0, 1),
                ("ModifiedDate", "date", 0, 1),
                ("ModifiedAuthor", "str", 0, 1),
            ],
        ),
        node(
            "VisualizationInfo",
            vec![("Guid", Attr::Guid, true)],
            vec![
                ("Components", "Components", 0, 1),
                ("OrthogonalCamera", "Ortho", 0, 1),
                ("PerspectiveCamera", "Persp", 0, 1),
                ("Lines", "Lines", 0, 1),
                ("ClippingPlanes", "Clips", 0, 1),
            ],
        ),
        node(
            "Components",
            vec![],
            vec![
                ("ViewSetupHints", "Hints", 0, 1),
                ("Selection", "Sel", 0, 1),
                ("Visibility", "Vis", 1, 1),
                ("Coloring", "Coloring", 0, 1),
            ],
        ),
        node("Vis", vec![("DefaultVisibility", Attr::Bool, false)], vec![("Exceptions", "Sel", 0, 1)]),
    ]);
    t
}

fn v30() -> Vec<Ty> {
    let mut t = common(BcfVersion::V30);
    let list = |name: &'static str, item: &'static str, ty: &'static str| {
        node(name, vec![], vec![(item, ty, 0, MANY)])
    };
    t.extend([
        node("Version", vec![("VersionId", Attr::Str, true)], vec![]),
        node("Header", vec![], vec![("Files", "Files", 0, 1)]),
        list("Files", "File", "File"),
        node("Markup", vec![], vec![("Header", "Header", 0, 1), ("Topic", "Topic", 1, 1)]),
        node(
            "Topic",
            vec![
                ("Guid", Attr::Guid, true),
                ("ServerAssignedId", Attr::Str, false),
                ("TopicType", Attr::NonBlank, true),
                ("TopicStatus", Attr::NonBlank, true),
            ],
            vec![
                ("ReferenceLinks", "ReferenceLinks", 0, 1),
                ("Title", "nb", 1, 1),
                ("Priority", "nb", 0, 1),
                ("Index", "int", 0, 1),
                ("Labels", "Labels", 0, 1),
                ("CreationDate", "date", 1, 1),
                ("CreationAuthor", "nb", 1, 1),
                ("ModifiedDate", "date", 0, 1),
                ("ModifiedAuthor", "nb", 0, 1),
                ("DueDate", "date", 0, 1),
                ("AssignedTo", "nb", 0, 1),
                ("Stage", "nb", 0, 1),
                ("Description", "str", 0, 1),
                ("BimSnippet", "BimSnippet", 0, 1),
                ("DocumentReferences", "DocRefs", 0, 1),
                ("RelatedTopics", "RelatedTopics", 0, 1),
                ("Comments", "Comments", 0, 1),
                ("Viewpoints", "ViewPoints", 0, 1),
            ],
        ),
        list("ReferenceLinks", "ReferenceLink", "str"),
        list("Labels", "Label", "nb"),
        list("DocRefs", "DocumentReference", "DocRef"),
        Ty {
            choice: vec!["DocumentGuid", "Url"],
            ..node(
                "DocRef",
                vec![("Guid", Attr::Guid, true)],
                vec![("DocumentGuid", "guid", 0, 1), ("Url", "nb", 0, 1), ("Description", "str", 0, 1)],
            )
        },
        node("DocumentInfo", vec![], vec![("Documents", "Documents", 0, 1)]),
        list("Documents", "Document", "Document"),
        node(
            "Document",
            vec![("Guid", Attr::Guid, true)],
            vec![("Filename", "nb", 1, 1), ("Description", "str", 0, 1)],
        ),
        list("RelatedTopics", "RelatedTopic", "Ref"),
        list("Comments", "Comment", "Comment"),
        list("ViewPoints", "ViewPoint", "ViewPoint"),
        node(
            "Comment",
            vec![("Guid", Attr::Guid, true)],
            vec![
                ("Date", "date", 1, 1),
                ("Author", "nb", 1, 1),
                ("Comment", "nb", 0, 1),
                ("Viewpoint", "Ref", 0, 1),
                ("ModifiedDate", "date", 0, 1),
                ("ModifiedAuthor", "nb", 0, 1),
            ],
        ),
        Ty {
            choice: vec!["OrthogonalCamera", "PerspectiveCamera"],
            ..node(
                "VisualizationInfo",
                vec![("Guid", Attr::Guid, true)],
                vec![
                    ("Components", "Components", 0, 1),
                    ("OrthogonalCamera", "Ortho", 0, 1),
                    ("PerspectiveCamera", "Persp", 0, 1),
                    ("Lines", "Lines", 0, 1),
                    ("ClippingPlanes", "Clips", 0, 1),
                ],
            )
        },
        node(
            "Components",
            vec![],
            vec![("Selection", "Sel", 0, 1), ("Visibility", "Vis", 0, 1), ("Coloring", "Coloring", 0, 1)],
        ),
        node(
            "Vis",
            vec![("DefaultVisibility", Attr::Bool, false)],
            vec![("ViewSetupHints", "Hints", 0, 1), ("Exceptions", "Sel", 0, 1)],
        ),
        node(
            "Extensions",
            vec![],
            vec![
                ("TopicTypes", "TopicTypes", 0, 1),
                ("TopicStatuses", "TopicStatuses", 0, 1),
                ("Priorities", "Priorities", 0, 1),
                ("TopicLabels", "TopicLabels", 0, 1),
                ("Users", "Users", 0, 1),
                ("SnippetTypes", "SnippetTypes", 0, 1),
                ("Stages", "Stages", 0, 1),
            ],
        ),
        list("TopicTypes", "TopicType", "nb"),
        list("TopicStatuses", "TopicStatus", "nb"),
        list("Priorities", "Priority", "nb"),
        list("TopicLabels", "TopicLabel", "nb"),
        list("Users", "User", "nb"),
        list("SnippetTypes", "SnippetType", "nb"),
        list("Stages", "Stage", "nb"),
    ]);
    t
}

fn is_guid(s: &str, v: BcfVersion) -> bool {
    let b = s.as_bytes();
    s.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => match v {
                BcfVersion::V30 => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
                BcfVersion::V21 => c.is_ascii_hexdigit(),
            },
        })
}

fn is_double(s: &str) -> Option<f64> {
    let ok = !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit() || b"+-.eE".contains(&c));
    // num-lint: xs:double is always dot-decimal
    ok.then(|| s.parse::<f64>().ok()).flatten().filter(|v| v.is_finite())
}

fn is_date(s: &str) -> bool {
    super::is_datetime(s)
}

fn leaf_ok(l: Leaf, text: &str, v: BcfVersion) -> bool {
    let t = text.trim();
    match l {
        Leaf::Str => true,
        Leaf::NonBlank => !t.is_empty(),
        Leaf::Double => is_double(t).is_some(),
        Leaf::Range(lo, hi, incl) => is_double(t).is_some_and(|x| {
            if incl {
                (lo..=hi).contains(&x)
            } else {
                x > lo && x < hi
            }
        }),
        Leaf::Positive => is_double(t).is_some_and(|x| x > 0.0),
        Leaf::Int => t.parse::<i64>().is_ok(),
        Leaf::Date => is_date(t),
        Leaf::Guid => is_guid(t, v),
    }
}

fn attr_ok(a: Attr, val: &str, v: BcfVersion) -> bool {
    match a {
        Attr::Str => true,
        Attr::NonBlank => !val.trim().is_empty(),
        Attr::Guid => is_guid(val, v),
        Attr::IfcGuid => super::write::is_ifc_guid(val),
        Attr::Bool => matches!(val, "true" | "false" | "1" | "0"),
    }
}

fn check_node(types: &[Ty], ty: &str, n: &Node, v: BcfVersion, path: &str) -> Result<(), String> {
    let path = format!("{path}/{}", n.name);
    let t = types
        .iter()
        .find(|t| t.name == ty)
        .ok_or_else(|| format!("{path}: no type {ty}"))?;
    for (k, val) in &n.attrs {
        // the reader keeps local names: `xmlns:xsi` reads as `xsi`
        let ns = ["xsi", "xsd", "schemaLocation", "noNamespaceSchemaLocation"];
        if k.starts_with("xmlns") || ns.contains(&k.as_str()) {
            continue;
        }
        let Some((_, a, _)) = t.attrs.iter().find(|x| x.0 == k.as_str()) else {
            return Err(format!("{path}: attribute {k} is not allowed"));
        };
        if !attr_ok(*a, val, v) {
            return Err(format!("{path}: attribute {k}=\"{val}\" is not a valid {a:?}"));
        }
    }
    for (k, _, req) in &t.attrs {
        if *req && n.attr(k).is_none() {
            return Err(format!("{path}: attribute {k} is required"));
        }
    }
    if let Some(l) = t.leaf {
        if !n.kids.is_empty() {
            return Err(format!("{path}: a {l:?} value has no child elements"));
        }
        if !leaf_ok(l, &n.text, v) {
            return Err(format!("{path}: \"{}\" is not a valid {l:?}", n.text));
        }
        return Ok(());
    }
    if !n.text.trim().is_empty() {
        return Err(format!("{path}: text where only elements belong"));
    }
    let mut i = 0;
    for (name, kt, min, max) in &t.kids {
        let mut count = 0;
        while i < n.kids.len() && n.kids[i].name == *name {
            if count == *max {
                return Err(format!("{path}: more than {max} {name}"));
            }
            check_node(types, kt, &n.kids[i], v, &path)?;
            count += 1;
            i += 1;
        }
        if count < *min {
            return Err(format!("{path}: {name} is required here"));
        }
    }
    if let Some(k) = n.kids.get(i) {
        return Err(format!("{path}: {} is not allowed here (order or name)", k.name));
    }
    if !t.choice.is_empty() {
        let found = n.kids.iter().filter(|k| t.choice.iter().any(|c| *c == k.name)).count();
        if found != 1 {
            return Err(format!("{path}: exactly one of {:?} is required", t.choice));
        }
    }
    Ok(())
}

/// Checks one file of a BCF zip of version `v`.
pub(super) fn check(v: BcfVersion, f: File, text: &str) -> Result<(), String> {
    let root = xml::parse(text).map_err(|e| e.to_string())?;
    let types = match v {
        BcfVersion::V21 => v21(),
        BcfVersion::V30 => v30(),
    };
    let want = match f {
        File::Version => "Version",
        File::Markup => "Markup",
        File::Visinfo => "VisualizationInfo",
        File::Extensions => "Extensions",
        File::Documents => "DocumentInfo",
    };
    if root.name != want {
        return Err(format!("root is {}, not {want}", root.name));
    }
    check_node(&types, want, &root, v, "")
}

/// Checks every XML file of a BCF zip; returns how many were checked.
pub(super) fn check_zip(v: BcfVersion, files: &std::collections::BTreeMap<String, Vec<u8>>) -> Result<usize, String> {
    let mut n = 0;
    for (name, bytes) in files {
        let f = if name == "bcf.version" {
            File::Version
        } else if name == "extensions.xml" {
            File::Extensions
        } else if name == "documents.xml" {
            File::Documents
        } else if name.ends_with(".bcf") {
            File::Markup
        } else if name.ends_with(".bcfv") {
            File::Visinfo
        } else {
            continue;
        };
        let text = std::str::from_utf8(bytes).map_err(|_| format!("{name}: not UTF-8"))?;
        check(v, f, text).map_err(|e| format!("{name}: {e}"))?;
        n += 1;
    }
    Ok(n)
}
