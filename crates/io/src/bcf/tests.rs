// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::io::{Cursor, Write};

use ddc_model::{
    ClipPlane, LayerSpec, Model, Orbit, Topic, TopicComment, TopicElem, Viewpoint, WallSpec,
};

use super::*;

const GUID: &str = "2N1o$wCUz9Tvg_BaDNrs0M";

fn model(walls: usize) -> (Model, Vec<i64>) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let t = m
        .add_wall_type(
            "w",
            &[LayerSpec {
                material: "brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let ids = (0..walls)
        .map(|i| {
            m.add_wall(&WallSpec {
                a: (0.0, i as f64 * 2.0),
                b: (5.0, i as f64 * 2.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap()
        })
        .collect();
    (m, ids)
}

fn link(m: &Model, guid: &str, wall: i64) {
    m.sql()
        .execute(
            "INSERT INTO ext_ref(source,guid,kind,ref) VALUES ('s',?1,'wall',?2)",
            rusqlite::params![guid, wall],
        )
        .unwrap();
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (n, b) in entries {
        w.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(b).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn png() -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    b.extend((0..=255u8).cycle().take(3000));
    b
}

fn full_topic(wall: i64) -> Topic {
    let mut t = Topic::new("Duct crosses beam <A&B> \"x\"");
    t.guid = "0f8fad5b-d9cb-469f-a165-70867728950e".into();
    t.status = "in_progress".into();
    t.priority = "high".into();
    t.assignee = "Anna Petrova".into();
    t.author = "Bob".into();
    t.description = "Line one\nLine two <tag> & more\n\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442}".into();
    t.created = 1_760_000_000;
    t.modified = 1_760_003_600;
    t.elements = vec![
        TopicElem::local("wall", wall),
        TopicElem::external("3vB$Unmatched0000000000"),
    ];
    t.comments = vec![
        TopicComment {
            guid: "c0000000-0000-4000-8000-000000000001".into(),
            ts: 1_760_000_100,
            author: "Carol".into(),
            text: "first <b>".into(),
        },
        TopicComment {
            guid: "c0000000-0000-4000-8000-000000000002".into(),
            ts: 1_760_000_200,
            author: "Dan".into(),
            text: "second\nline".into(),
        },
    ];
    t.viewpoint = Some(Viewpoint {
        eye: [10.5, -3.25, 4.0],
        dir: [-0.5, 0.5, -0.7071067811865476],
        up: [0.0, 0.0, 1.0],
        ortho: false,
        fov: 60.0,
        scale: 0.0,
        aspect: 1.5,
        clip: ClipPlane::of_box([-2.5, 0.0, -0.25], [12.0, 8.75, 3.1]),
        orbit: None,
    });
    t.snapshot = Some(png());
    t.bcf = TopicBcf {
        topic_type: "Clash".into(),
        labels: vec!["MEP".into(), "Level <2>".into()],
        due: "2026-12-01T17:00:00+03:00".into(),
        stage: "Design".into(),
        links: vec!["https://example.org/issue?a=1&b=2".into()],
        docs: vec![
            TopicDoc {
                guid: "d0000000-0000-4000-8000-000000000001".into(),
                url: "https://example.org/spec.pdf".into(),
                description: "Spec".into(),
                file: None,
            },
            TopicDoc {
                guid: "d0000000-0000-4000-8000-000000000002".into(),
                url: String::new(),
                description: "Site note".into(),
                file: Some(TopicDocFile {
                    guid: "f0000000-0000-4000-8000-000000000003".into(),
                    name: "note.txt".into(),
                    data: b"bytes\0and more".to_vec(),
                }),
            },
        ],
        // the file's own words, kept while they mean the stored status and priority
        status_word: "Assigned".into(),
        priority_word: "Major".into(),
    };
    t
}

fn same_fields(a: &Topic, b: &Topic) {
    assert_eq!(a.guid, b.guid);
    assert_eq!(a.title, b.title);
    assert_eq!(a.status, b.status);
    assert_eq!(a.priority, b.priority);
    assert_eq!(a.assignee, b.assignee);
    assert_eq!(a.author, b.author);
    assert_eq!(a.description, b.description);
    assert_eq!(a.created, b.created);
    assert_eq!(a.modified, b.modified);
    assert_eq!(a.viewpoint, b.viewpoint);
    assert_eq!(a.snapshot, b.snapshot);
    assert_eq!(a.comments, b.comments);
    assert_eq!(a.bcf, b.bcf);
    let g =|t: &Topic| -> Vec<(Option<String>, bool)> {
        t.elements.iter().map(|e| (e.guid.clone(), e.matched())).collect()
    };
    assert_eq!(g(a), g(b));
}

#[test]
fn export_then_import_keeps_every_field_in_both_versions() {
    for v in [BcfVersion::V21, BcfVersion::V30] {
        let (a, wa) = model(1);
        link(&a, GUID, wa[0]);
        let mut t = full_topic(wa[0]);
        t.elements[0].guid = Some(GUID.into());
        let ida = a.topic_add(&t).unwrap();
        let bytes = export_bytes(&a, None, v).unwrap();

        // another project: the wall has another id, only the guid ties them
        let (b, wb) = model(2);
        link(&b, GUID, wb[1]);
        let rep = import_bytes(&b, &bytes).unwrap();
        assert_eq!((rep.created, rep.updated), (1, 0), "{v:?}");
        assert_eq!(rep.version, v.id());
        assert_eq!(rep.elements_matched, 1);
        assert_eq!(rep.unmatched.len(), 1);
        assert_eq!(rep.unmatched[0].reference, "3vB$Unmatched0000000000");

        let mut before = a.topic_get(ida).unwrap();
        let after = b.topic_get(b.topic_by_guid(&t.guid).unwrap().unwrap()).unwrap();
        if v == BcfVersion::V21 {
            // the 2.1 camera schema has no aspect ratio: another project reads none
            before.viewpoint.as_mut().unwrap().aspect = 0.0;
        }
        same_fields(&before, &after);
        assert_eq!(after.elements[0].id, wb[1], "matched by guid, not by id");
        assert_eq!(after.source, "bcf");
    }
}

#[test]
fn importing_the_same_file_twice_updates_instead_of_duplicating() {
    let (a, w) = model(1);
    a.topic_add(&full_topic(w[0])).unwrap();
    let bytes = export_bytes(&a, None, BcfVersion::V21).unwrap();
    let r = import_bytes(&a, &bytes).unwrap();
    assert_eq!((r.created, r.updated), (0, 1));
    assert_eq!(a.topic_list(None).unwrap().len(), 1);
    let id = a.topic_by_guid("0f8fad5b-d9cb-469f-a165-70867728950e").unwrap().unwrap();
    assert_eq!(a.topic_get(id).unwrap().comments.len(), 2, "comments are not doubled");
}

#[test]
fn elements_without_a_guid_match_by_table_and_id_in_the_same_project() {
    let (a, w) = model(2);
    let mut t = Topic::new("no guid");
    t.elements = vec![TopicElem::local("wall", w[1])];
    let id = a.topic_add(&t).unwrap();
    let bytes = export_bytes(&a, Some(&[id]), BcfVersion::V30).unwrap();
    a.topic_delete(id).unwrap();
    let rep = import_bytes(&a, &bytes).unwrap();
    assert_eq!((rep.elements_matched, rep.unmatched.len()), (1, 0));
    let got = a.topic_get(a.topic_list(None).unwrap()[0].id).unwrap();
    assert_eq!((got.elements[0].table.as_str(), got.elements[0].id), ("wall", w[1]));

    // another project ignores the local reference and reports it
    let (b, _) = model(2);
    let rep = import_bytes(&b, &bytes).unwrap();
    assert_eq!(rep.elements_matched, 0);
    assert_eq!(rep.warnings.len(), 1, "the dropped component is mentioned");
}

#[test]
fn a_local_reference_to_a_deleted_element_is_reported() {
    let (a, _) = model(1);
    let mut t = Topic::new("gone");
    t.elements = vec![TopicElem::local("wall", 9999)];
    let id = a.topic_add(&t).unwrap();
    let bytes = export_bytes(&a, Some(&[id]), BcfVersion::V21).unwrap();
    a.topic_delete(id).unwrap();
    let rep = import_bytes(&a, &bytes).unwrap();
    assert_eq!(rep.unmatched.len(), 1);
    assert_eq!(rep.unmatched[0].reference, "wall:9999");
    assert!(a.topic_get(a.topic_list(None).unwrap()[0].id).unwrap().elements.is_empty());
}

#[test]
fn an_issue_without_camera_or_elements_writes_no_viewpoint() {
    let (a, _) = model(1);
    a.topic_add(&Topic::new("plain")).unwrap();
    let bytes = export_bytes(&a, None, BcfVersion::V21).unwrap();
    let files = read_zip(&bytes).unwrap();
    assert!(files.keys().any(|k| k.ends_with("markup.bcf")));
    assert!(!files.keys().any(|k| k.ends_with(".bcfv") || k.ends_with(".png")));
    let (b, _) = model(1);
    assert_eq!(import_bytes(&b, &bytes).unwrap().created, 1);
}

#[test]
fn an_orthogonal_camera_survives() {
    let (a, _) = model(1);
    let mut t = Topic::new("ortho");
    t.viewpoint = Some(Viewpoint {
        eye: [1.0, 2.0, 30.0],
        dir: [0.0, 0.0, -1.0],
        up: [0.0, 1.0, 0.0],
        ortho: true,
        fov: 45.0,
        scale: 12.5,
        ..Viewpoint::default()
    });
    let id = a.topic_add(&t).unwrap();
    let mut want = a.topic_get(id).unwrap().viewpoint;
    // 3.0 requires an aspect ratio: a view without one is written with the default
    want.as_mut().unwrap().aspect = write::DEFAULT_ASPECT;
    let bytes = export_bytes(&a, None, BcfVersion::V30).unwrap();
    let (b, _) = model(1);
    import_bytes(&b, &bytes).unwrap();
    let got = b.topic_get(b.topic_list(None).unwrap()[0].id).unwrap();
    assert_eq!(got.viewpoint, want);
}

#[test]
fn version_30_nests_comments_and_viewpoints_in_the_topic() {
    let (a, w) = model(1);
    a.topic_add(&full_topic(w[0])).unwrap();
    let bytes = export_bytes(&a, None, BcfVersion::V30).unwrap();
    let files = read_zip(&bytes).unwrap();
    let ver = String::from_utf8(files["bcf.version"].clone()).unwrap();
    assert!(ver.contains("VersionId=\"3.0\""));
    let key = files.keys().find(|k| k.ends_with("markup.bcf")).unwrap();
    let root = xml::parse(std::str::from_utf8(&files[key]).unwrap()).unwrap();
    let top = root.kid("Topic").unwrap();
    assert!(top.kid("Comments").is_some() && top.kid("Viewpoints").is_some());
    assert!(root.kid("Comment").is_none());
}

#[test]
fn a_foreign_file_with_namespaces_and_other_vocabulary_is_read() {
    let markup = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
        <Markup xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
        xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">\
        <Header><File IfcProject=\"p\"><Filename>a.ifc</Filename></File></Header>\
        <Topic Guid=\"aaaaaaaa-0000-4000-8000-000000000001\" TopicType=\"Clash\" TopicStatus=\"Active\">\
        <Title>Wall vs pipe</Title><Priority>Medium</Priority>\
        <CreationDate>2026-03-01T10:00:00.123+02:00</CreationDate>\
        <CreationAuthor>x@y.z</CreationAuthor><AssignedTo>me</AssignedTo>\
        <Description>d</Description></Topic>\
        <Comment Guid=\"cccccccc-0000-4000-8000-000000000001\"><Date>2026-03-01T11:00:00Z</Date>\
        <Author>x@y.z</Author><Comment>hello</Comment></Comment>\
        <Viewpoints Guid=\"v1\"><Viewpoint>vp.bcfv</Viewpoint><Snapshot>s.png</Snapshot></Viewpoints>\
        </Markup>";
    let vp = format!(
        "<VisualizationInfo Guid=\"v1\"><Components><Selection>\
         <Component IfcGuid=\"{GUID}\"/><Component IfcGuid=\"nomatch\"/></Selection></Components>\
         <PerspectiveCamera><CameraViewPoint><X>1</X><Y>2</Y><Z>3</Z></CameraViewPoint>\
         <CameraDirection><X>0</X><Y>1</Y><Z>0</Z></CameraDirection>\
         <CameraUpVector><X>0</X><Y>0</Y><Z>1</Z></CameraUpVector>\
         <FieldOfView>50</FieldOfView></PerspectiveCamera></VisualizationInfo>"
    );
    let snap = png();
    let z = zip_of(&[
        ("bcf.version", b"<Version VersionId=\"2.1\"/>"),
        ("aaaaaaaa-0000-4000-8000-000000000001/markup.bcf", markup.as_bytes()),
        ("aaaaaaaa-0000-4000-8000-000000000001/vp.bcfv", vp.as_bytes()),
        ("aaaaaaaa-0000-4000-8000-000000000001/s.png", &snap),
    ]);
    let (m, w) = model(1);
    link(&m, GUID, w[0]);
    let rep = import_bytes(&m, &z).unwrap();
    assert_eq!((rep.created, rep.elements_matched, rep.unmatched.len()), (1, 1, 1));
    let t = m.topic_get(m.topic_list(None).unwrap()[0].id).unwrap();
    assert_eq!(t.status, "open");
    assert_eq!(t.priority, "normal");
    assert_eq!(t.created, parse_iso("2026-03-01T08:00:00Z").unwrap());
    assert_eq!(t.comments.len(), 1);
    assert_eq!(t.viewpoint.as_ref().unwrap().fov, 50.0);
    assert_eq!(t.snapshot.as_deref(), Some(snap.as_slice()));
    assert_eq!(t.elements.len(), 2);
}

#[test]
fn foreign_document_references_and_words_are_kept_and_written_back() {
    let markup = "<Markup><Topic Guid=\"aaaaaaaa-0000-4000-8000-000000000002\" TopicType=\"Issue\" \
        TopicStatus=\"Active\"><ReferenceLink>https://x.org/1</ReferenceLink>\
        <Title>Docs</Title><Priority>High</Priority><Labels>A</Labels><Labels>B</Labels>\
        <CreationDate>2026-03-01T10:00:00Z</CreationDate><CreationAuthor>x</CreationAuthor>\
        <DueDate>next week</DueDate><Stage>Tender</Stage>\
        <DocumentReference isExternal=\"false\"><ReferencedDocument>../docs/a.pdf</ReferencedDocument>\
        <Description>in the file</Description></DocumentReference>\
        <DocumentReference><ReferencedDocument>https://x.org/b.pdf</ReferencedDocument></DocumentReference>\
        <DocumentReference isExternal=\"false\"><ReferencedDocument>../../out.pdf</ReferencedDocument></DocumentReference>\
        <DocumentReference isExternal=\"false\"><ReferencedDocument>C:\\docs\\c.pdf</ReferencedDocument></DocumentReference>\
        </Topic></Markup>";
    let z = zip_of(&[
        ("bcf.version", b"<Version VersionId=\"2.1\"/>"),
        ("t/markup.bcf", markup.as_bytes()),
        ("docs/a.pdf", b"%PDF-a"),
    ]);
    let (m, _) = model(1);
    let rep = import_bytes(&m, &z).unwrap();
    assert_eq!(rep.created, 1);
    assert!(rep.warnings.iter().any(|w| w.contains("next week")), "{:?}", rep.warnings);
    assert_eq!(rep.warnings.iter().filter(|w| w.contains("missing")).count(), 2, "{:?}", rep.warnings);
    let id = m.topic_list(None).unwrap()[0].id;
    let t = m.topic_get(id).unwrap();
    let b = &t.bcf;
    assert_eq!(b.topic_type, "", "the default type is not stored");
    assert_eq!((b.status_word.as_str(), b.priority_word.as_str()), ("Active", ""));
    assert_eq!(b.labels, vec!["A".to_string(), "B".to_string()]);
    assert_eq!((b.due.as_str(), b.stage.as_str()), ("", "Tender"));
    assert_eq!(b.links, vec!["https://x.org/1".to_string()]);
    assert_eq!(b.docs.len(), 2);
    let f = b.docs[0].file.as_ref().unwrap();
    assert_eq!((f.name.as_str(), f.data.as_slice()), ("a.pdf", &b"%PDF-a"[..]));
    assert_eq!(b.docs[0].description, "in the file");
    assert_eq!(b.docs[1].url, "https://x.org/b.pdf");

    // written back in either version, read again: the same fields
    for v in [BcfVersion::V21, BcfVersion::V30] {
        let bytes = export_bytes(&m, None, v).unwrap();
        let files = read_zip(&bytes).unwrap();
        schema::check_zip(v, &files).unwrap_or_else(|e| panic!("{v:?}: {e}"));
        let markup = files.keys().find(|k| k.ends_with("markup.bcf")).unwrap();
        let text = std::str::from_utf8(&files[markup]).unwrap();
        assert!(text.contains("TopicStatus=\"Active\""), "{v:?}: the file's word goes back");
        let (n, _) = model(1);
        import_bytes(&n, &bytes).unwrap();
        let back = n.topic_get(n.topic_list(None).unwrap()[0].id).unwrap();
        assert_eq!(back.bcf, t.bcf, "{v:?}");
    }
    // a status changed in the editor drops the file's word for it
    let mut e = m.topic_get(id).unwrap();
    e.status = "closed".into();
    m.topic_import(&e).unwrap();
    let bytes = export_bytes(&m, None, BcfVersion::V30).unwrap();
    let files = read_zip(&bytes).unwrap();
    let markup = files.keys().find(|k| k.ends_with("markup.bcf")).unwrap();
    assert!(std::str::from_utf8(&files[markup]).unwrap().contains("TopicStatus=\"Closed\""));
}

#[test]
fn malformed_input_is_an_error_not_a_panic() {
    let (m, _) = model(1);
    let good = zip_of(&[("bcf.version", b"<Version VersionId=\"2.1\"/>")]);
    let truncated = good[..good.len() / 2].to_vec();
    let cases: Vec<Vec<u8>> = vec![
        vec![],
        b"not a zip at all".to_vec(),
        vec![0x50, 0x4b, 0x03, 0x04, 1, 2, 3],
        truncated,
        zip_of(&[]),
        zip_of(&[("x/markup.bcf", b"<Markup/>")]),
        zip_of(&[("bcf.version", b"<Version VersionId=\"9.9\"/>")]),
        zip_of(&[("bcf.version", b"<Version")]),
        zip_of(&[
            ("bcf.version", b"<Version VersionId=\"2.1\"/>"),
            ("a/markup.bcf", b"<Markup><Topic Guid=\"g\"><Title>t</Topic>"),
        ]),
        zip_of(&[
            ("bcf.version", b"<Version VersionId=\"2.1\"/>"),
            ("a/markup.bcf", &[0xff, 0xfe, 0x00]),
        ]),
    ];
    for (i, c) in cases.iter().enumerate() {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| import_bytes(&m, c)));
        assert!(matches!(r, Ok(Err(_))), "case {i}: {r:?}");
    }
    assert!(m.topic_list(None).unwrap().is_empty());
}

#[test]
fn a_topic_without_a_title_is_skipped_with_a_warning() {
    let z = zip_of(&[
        ("bcf.version", b"<Version VersionId=\"2.1\"/>"),
        ("a/markup.bcf", b"<Markup><Topic Guid=\"g\"></Topic></Markup>"),
    ]);
    let (m, _) = model(1);
    let r = import_bytes(&m, &z).unwrap();
    assert_eq!(r.created, 0);
    assert_eq!(r.warnings.len(), 1);
}

#[test]
fn path_traversal_in_entry_names_is_rejected() {
    let (m, _) = model(1);
    let ver: &[u8] = b"<Version VersionId=\"2.1\"/>";
    for bad in [
        "../evil/markup.bcf",
        "a/../../evil.txt",
        "/abs/markup.bcf",
        "\\abs\\markup.bcf",
        "..\\evil\\markup.bcf",
        "C:/evil/markup.bcf",
        "C:\\evil\\markup.bcf",
        "a/b/../markup.bcf",
    ] {
        let z = zip_of(&[("bcf.version", ver), (bad, b"<Markup/>")]);
        let r = import_bytes(&m, &z);
        assert!(matches!(r, Err(BcfError::UnsafePath(_))), "{bad}: {r:?}");
    }
    assert!(m.topic_list(None).unwrap().is_empty());
}

#[test]
fn a_viewpoint_reference_that_leaves_the_folder_is_rejected() {
    let (m, _) = model(1);
    for r in ["../vp.bcfv", "/etc/passwd", "..\\..\\x"] {
        let markup = format!(
            "<Markup><Topic Guid=\"g\"><Title>t</Title></Topic>\
             <Viewpoints Guid=\"v\"><Viewpoint>{r}</Viewpoint></Viewpoints></Markup>"
        );
        let z = zip_of(&[
            ("bcf.version", b"<Version VersionId=\"2.1\"/>"),
            ("a/markup.bcf", markup.as_bytes()),
        ]);
        let res = import_bytes(&m, &z);
        assert!(matches!(res, Err(BcfError::UnsafePath(_))), "{r}: {res:?}");
    }
    let markup = "<Markup><Topic Guid=\"g\"><Title>t</Title></Topic>\
        <Viewpoints Guid=\"v\"><Snapshot>../s.png</Snapshot></Viewpoints></Markup>";
    let z = zip_of(&[
        ("bcf.version", b"<Version VersionId=\"2.1\"/>"),
        ("a/markup.bcf", markup.as_bytes()),
    ]);
    assert!(matches!(import_bytes(&m, &z), Err(BcfError::UnsafePath(_))));
}

#[test]
fn a_doctype_is_refused() {
    let z = zip_of(&[(
        "bcf.version",
        b"<!DOCTYPE v [<!ENTITY a \"b\">]><Version VersionId=\"2.1\"/>",
    )]);
    let (m, _) = model(1);
    assert!(matches!(import_bytes(&m, &z), Err(BcfError::Xml(_))));
}

#[test]
fn a_snapshot_that_is_not_an_image_is_dropped_with_a_warning() {
    let markup = "<Markup><Topic Guid=\"g\"><Title>t</Title></Topic>\
        <Viewpoints Guid=\"v\"><Snapshot>s.png</Snapshot></Viewpoints></Markup>";
    let z = zip_of(&[
        ("bcf.version", b"<Version VersionId=\"2.1\"/>"),
        ("a/markup.bcf", markup.as_bytes()),
        ("a/s.png", b"this is text"),
    ]);
    let (m, _) = model(1);
    let r = import_bytes(&m, &z).unwrap();
    assert_eq!((r.created, r.warnings.len()), (1, 1));
    let t = m.topic_get(m.topic_list(None).unwrap()[0].id).unwrap();
    assert!(t.snapshot.is_none());
}

#[test]
fn times_convert_both_ways() {
    assert_eq!(iso(0), "1970-01-01T00:00:00Z");
    assert_eq!(iso(1_760_003_600), "2025-10-09T09:53:20Z");
    for ts in [0, 86_399, 951_782_400, 1_760_003_600, 4_102_444_800] {
        assert_eq!(parse_iso(&iso(ts)), Some(ts));
    }
    assert_eq!(
        parse_iso("2026-11-08T12:00:00+02:00"),
        parse_iso("2026-11-08T10:00:00Z")
    );
    assert_eq!(
        parse_iso("2026-11-08T12:00:00-0530"),
        parse_iso("2026-11-08T17:30:00Z")
    );
    assert_eq!(parse_iso("2026-11-08"), parse_iso("2026-11-08T00:00:00Z"));
    for bad in ["", "x", "2026-13-01", "2026-11-08T25:00:00Z", "2026-11-08T10:00:00+", "2026-11"] {
        assert_eq!(parse_iso(bad), None, "{bad}");
    }
}

#[test]
fn status_and_priority_words_map_both_ways() {
    for s in ["open", "in_progress", "resolved", "closed"] {
        assert_eq!(status_from(&status_label(s)), s);
    }
    for p in ["low", "normal", "high", "critical"] {
        assert_eq!(priority_from(&priority_label(p)), p);
    }
    assert_eq!(status_from("ToDo"), "ToDo");
    assert_eq!(status_from("In-Progress"), "in_progress");
}

#[test]
fn export_of_a_selection_of_issues_writes_only_those() {
    let (a, _) = model(1);
    let i1 = a.topic_add(&Topic::new("one")).unwrap();
    let _i2 = a.topic_add(&Topic::new("two")).unwrap();
    let bytes = export_bytes(&a, Some(&[i1]), BcfVersion::V21).unwrap();
    let (b, _) = model(1);
    assert_eq!(import_bytes(&b, &bytes).unwrap().created, 1);
    assert_eq!(b.topic_list(None).unwrap()[0].title, "one");
    assert!(export_bytes(&a, Some(&[9999]), BcfVersion::V21).is_err());
}

// ---- schema conformance, lossless round trip, camera ----

fn orbit(ortho: bool, fov_deg: f64) -> Orbit {
    Orbit {
        target: [12.25, -7.5, 1.75],
        yaw: 0.83,
        pitch: 0.41,
        distance: 23.5,
        fov_y: fov_deg.to_radians(),
        ortho,
    }
}

/// Every kind of issue the editor writes: full, camera-less with elements, snapshot only,
/// orthographic, no author, a non-IFC guid, a wide 2.1-incompatible field of view.
fn rich_project() -> (Model, Vec<i64>) {
    let (a, w) = model(2);
    link(&a, GUID, w[0]);
    let mut full = full_topic(w[0]);
    full.elements[0].guid = Some(GUID.into());
    let mut ids = vec![a.topic_add(&full).unwrap()];
    let mut bare = Topic::new("elements only");
    bare.elements = vec![TopicElem::local("wall", w[1])];
    bare.author = String::new();
    ids.push(a.topic_add(&bare).unwrap());
    let mut snap = Topic::new("snapshot only");
    snap.snapshot = Some(png());
    ids.push(a.topic_add(&snap).unwrap());
    let mut ortho = Topic::new("plan view");
    let mut v = Viewpoint::from_orbit(&orbit(true, 45.0));
    v.aspect = 1.25;
    ortho.viewpoint = Some(v);
    ids.push(a.topic_add(&ortho).unwrap());
    let mut wide = Topic::new("wide lens");
    let mut v = Viewpoint::from_orbit(&orbit(false, 75.0));
    v.aspect = 1.4;
    v.clip = ClipPlane::of_box([0.0, 0.0, 0.0], [5.0, 5.0, 3.0]);
    v.clip.push(ClipPlane { at: [1.0, 2.0, 0.0], dir: [0.6, 0.8, 0.0] });
    wide.viewpoint = Some(v);
    wide.elements = vec![TopicElem::external("not-an-ifc-guid#7")];
    ids.push(a.topic_add(&wide).unwrap());
    (a, ids)
}

#[test]
fn every_exported_file_follows_the_schema() {
    for v in [BcfVersion::V21, BcfVersion::V30] {
        let (a, _) = rich_project();
        let files = read_zip(&export_bytes(&a, None, v).unwrap()).unwrap();
        let n = schema::check_zip(v, &files).unwrap_or_else(|e| panic!("{v:?}: {e}"));
        // version + 5 markups + 4 viewpoints (+ extensions and documents in 3.0)
        let want = if v == BcfVersion::V30 { 12 } else { 10 };
        assert_eq!(n, want, "{v:?}: {:?}", files.keys().collect::<Vec<_>>());
        assert_eq!(files.contains_key("extensions.xml"), v == BcfVersion::V30);
        assert_eq!(files.contains_key("documents.xml"), v == BcfVersion::V30);
        let doc = match v {
            BcfVersion::V21 => "documents/f0000000-0000-4000-8000-000000000003/note.txt",
            BcfVersion::V30 => "documents/f0000000-0000-4000-8000-000000000003",
        };
        assert_eq!(files.get(doc).map(Vec::as_slice), Some(&b"bytes\0and more"[..]), "{v:?}");
    }
}

#[test]
fn the_schema_check_catches_what_it_should() {
    use schema::{check, File};
    let v21 = BcfVersion::V21;
    let v30 = BcfVersion::V30;
    let cam = |extra: &str, fov: &str| {
        format!(
            "<VisualizationInfo Guid=\"0f8fad5b-d9cb-469f-a165-70867728950e\">\
             <PerspectiveCamera><CameraViewPoint><X>0</X><Y>0</Y><Z>0</Z></CameraViewPoint>\
             <CameraDirection><X>1</X><Y>0</Y><Z>0</Z></CameraDirection>\
             <CameraUpVector><X>0</X><Y>0</Y><Z>1</Z></CameraUpVector>\
             <FieldOfView>{fov}</FieldOfView>{extra}</PerspectiveCamera></VisualizationInfo>"
        )
    };
    assert!(check(v21, File::Visinfo, &cam("", "50")).is_ok());
    assert!(check(v21, File::Visinfo, &cam("", "70")).is_err(), "2.1 field of view range");
    assert!(check(v30, File::Visinfo, &cam("", "70")).is_err(), "3.0 needs AspectRatio");
    assert!(check(v30, File::Visinfo, &cam("<AspectRatio>1.5</AspectRatio>", "70")).is_ok());
    assert!(check(v30, File::Visinfo, &cam("<AspectRatio>0</AspectRatio>", "70")).is_err());
    let no_cam = "<VisualizationInfo Guid=\"0f8fad5b-d9cb-469f-a165-70867728950e\"/>";
    assert!(check(v21, File::Visinfo, no_cam).is_ok());
    assert!(check(v30, File::Visinfo, no_cam).is_err(), "3.0 needs a camera");
    let comp = |c: &str| {
        format!(
            "<VisualizationInfo Guid=\"0f8fad5b-d9cb-469f-a165-70867728950e\"><Components>\
             <Selection>{c}</Selection><Visibility/></Components></VisualizationInfo>"
        )
    };
    assert!(check(v21, File::Visinfo, &comp(&format!("<Component IfcGuid=\"{GUID}\"/>"))).is_ok());
    let attr = "<Component OriginatingSystem=\"x\" AuthoringToolId=\"wall:1\"/>";
    assert!(check(v21, File::Visinfo, &comp(attr)).is_err(), "ids are elements, not attributes");
    assert!(check(v21, File::Visinfo, &comp("<Component IfcGuid=\"short\"/>")).is_err());
    let markup = |guid: &str, order: &str| {
        format!(
            "<Markup><Topic Guid=\"{guid}\" TopicType=\"Issue\" TopicStatus=\"Open\">{order}\
             </Topic></Markup>"
        )
    };
    let good = "<Title>t</Title><CreationDate>2026-01-02T03:04:05Z</CreationDate>\
                <CreationAuthor>a</CreationAuthor>";
    let swapped = "<CreationDate>2026-01-02T03:04:05Z</CreationDate><Title>t</Title>\
                   <CreationAuthor>a</CreationAuthor>";
    let lower = "0f8fad5b-d9cb-469f-a165-70867728950e";
    let upper = "0F8FAD5B-D9CB-469F-A165-70867728950E";
    assert!(check(v30, File::Markup, &markup(lower, good)).is_ok());
    assert!(check(v30, File::Markup, &markup(lower, swapped)).is_err(), "order");
    assert!(check(v30, File::Markup, &markup(upper, good)).is_err(), "3.0 guids are lower case");
    assert!(check(v21, File::Markup, &markup(upper, good)).is_ok());
    let blank = good.replace("<CreationAuthor>a<", "<CreationAuthor> <");
    assert!(check(v30, File::Markup, &markup(lower, &blank)).is_err(), "3.0 non-blank");
    let bad_date = good.replace("03:04:05Z", "3:04");
    assert!(check(v21, File::Markup, &markup(lower, &bad_date)).is_err());
    assert!(check(v30, File::Version, "<Version VersionId=\"3.0\"/>").is_ok());
    assert!(check(
        v30,
        File::Version,
        "<Version VersionId=\"3.0\"><DetailedVersion>3.0</DetailedVersion></Version>"
    )
    .is_err());
    assert!(check(v21, File::Version, "<Version/>").is_err(), "VersionId is required");
}

/// Export then import into the same project gives back every issue exactly, the editor's own
/// orbit and aspect included: nothing is lost in either version.
#[test]
fn export_then_import_in_the_same_project_is_lossless() {
    for v in [BcfVersion::V21, BcfVersion::V30] {
        let (a, ids) = rich_project();
        // a camera taken in the editor carries its orbit
        let mut t = a.topic_get(ids[0]).unwrap();
        let mut vp = Viewpoint::from_orbit(&orbit(false, 45.0));
        vp.aspect = 1.6;
        vp.clip = ClipPlane::of_box([-1.0, -1.0, 0.0], [9.0, 9.0, 2.5]);
        t.viewpoint = Some(vp);
        a.topic_import(&t).unwrap();
        let before: Vec<Topic> = ids.iter().map(|i| a.topic_get(*i).unwrap()).collect();
        let bytes = export_bytes(&a, None, v).unwrap();
        let rep = import_bytes(&a, &bytes).unwrap();
        assert_eq!((rep.created, rep.updated), (0, ids.len()), "{v:?}");
        for (i, b) in ids.iter().zip(&before) {
            let mut after = a.topic_get(*i).unwrap();
            let mut want = b.clone();
            if v == BcfVersion::V30 && b.viewpoint.is_none() && !b.elements.is_empty() {
                // 3.0 needs a camera: the one that frames the elements is added
                let cam = after.viewpoint.take().expect("a framing camera");
                let framed = a.topic_frame_view(&b.elements).unwrap().map(|mut c| {
                    c.orbit = None;
                    c.aspect = write::DEFAULT_ASPECT;
                    c
                });
                assert_eq!(Some(cam), framed);
            }
            if v == BcfVersion::V21 && b.title == "wide lens" {
                // 2.1 allows 45..60 degrees: the lens is narrowed, the frame at the pivot kept
                let got = after.viewpoint.take().unwrap();
                let w = want.viewpoint.take().unwrap();
                assert_eq!(got.fov, 60.0);
                assert_eq!(got.clip, w.clip);
                assert!(got.same_camera(&write::fit_fov_21(&w), 1e-9));
            }
            after.modified = want.modified;
            assert_eq!(after.title, want.title, "{v:?}");
            same_fields(&want, &after);
            assert_eq!(after.viewpoint, want.viewpoint, "{v:?} {}", want.title);
        }
    }
}

#[test]
fn the_restored_camera_is_the_saved_one() {
    let (a, _) = model(1);
    for (ortho, fov) in [(false, 45.0), (false, 52.5), (true, 45.0)] {
        let o = orbit(ortho, fov);
        let mut t = Topic::new("cam");
        t.viewpoint = Some(Viewpoint::from_orbit(&o));
        let id = a.topic_add(&t).unwrap();
        for v in [BcfVersion::V21, BcfVersion::V30] {
            let bytes = export_bytes(&a, Some(&[id]), v).unwrap();
            // same project: the very orbit
            import_bytes(&a, &bytes).unwrap();
            let vp = a.topic_get(id).unwrap().viewpoint.unwrap();
            assert_eq!(vp.to_orbit(1.0), Some(o), "{v:?} {ortho} {fov}");
            // another project: the same picture
            let (b, _) = model(1);
            import_bytes(&b, &bytes).unwrap();
            let got = b.topic_get(b.topic_list(None).unwrap()[0].id).unwrap().viewpoint.unwrap();
            let back = got.to_orbit(o.distance).unwrap();
            assert!(Viewpoint::from_orbit(&back).same_camera(&Viewpoint::from_orbit(&o), 1e-9));
        }
        a.topic_delete(id).unwrap();
    }
}

#[test]
fn a_wide_lens_keeps_the_frame_at_the_pivot_in_21() {
    let mut v = Viewpoint::from_orbit(&orbit(false, 80.0));
    let o = v.orbit.unwrap();
    let f = write::fit_fov_21(&v);
    assert_eq!(f.fov, 60.0);
    // the visible height at the pivot is the same before and after
    let pivot = [
        v.eye[0] + v.dir[0] * o.distance,
        v.eye[1] + v.dir[1] * o.distance,
        v.eye[2] + v.dir[2] * o.distance,
    ];
    let d2 = (0..3).map(|i| (pivot[i] - f.eye[i]).powi(2)).sum::<f64>().sqrt();
    let h1 = o.distance * (80f64.to_radians() / 2.0).tan();
    let h2 = d2 * (60f64.to_radians() / 2.0).tan();
    assert!((h1 - h2).abs() < 1e-9, "{h1} {h2}");
    v.fov = 50.0;
    assert_eq!(write::fit_fov_21(&v), v, "inside the range nothing moves");
}

#[test]
fn a_view_with_a_section_box_and_no_aspect() {
    let (a, _) = model(1);
    let mut t = Topic::new("box");
    let mut v = Viewpoint::from_orbit(&orbit(false, 45.0));
    v.clip = ClipPlane::of_box([0.0, -1.0, 0.5], [4.0, 3.0, 2.5]);
    t.viewpoint = Some(v.clone());
    a.topic_add(&t).unwrap();
    let bytes = export_bytes(&a, None, BcfVersion::V30).unwrap();
    let (b, _) = model(1);
    import_bytes(&b, &bytes).unwrap();
    let got = b.topic_get(b.topic_list(None).unwrap()[0].id).unwrap().viewpoint.unwrap();
    assert_eq!(got.clip, v.clip);
    let bx = ddc_model::clip_box(&got.clip).unwrap();
    assert_eq!((bx.min, bx.max), ([0.0, -1.0, 0.5], [4.0, 3.0, 2.5]));
    assert_eq!(got.aspect, write::DEFAULT_ASPECT, "3.0 needs one: the default is written");
}
