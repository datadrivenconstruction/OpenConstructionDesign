// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! DWG -> model -> edit -> DXF text -> model again. Synthetic records go through the same
//! import as the DWG builder output; files from `DDC_DWG_CORPUS` are used when present.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::*;
use crate::dxf::{self, AttDef, Attrib, Block, Common, Entity, Insert, LwPolyline, LwVertex};
use ddc_model::Model;

const TABLES: [&str; 8] = [
    "d_line",
    "d_polyline",
    "d_vertex",
    "d_arc",
    "d_text",
    "d_insert",
    "d_attr",
    "d_attdef",
];

/// Read-only corpus of .dwg files with reference .dxf exports (outside the repo; optional).
fn corpus() -> Vec<(PathBuf, PathBuf)> {
    let Ok(root) = std::env::var("DDC_DWG_CORPUS").map(PathBuf::from) else {
        return vec![];
    };
    let Ok(rd) = std::fs::read_dir(&root) else {
        return vec![];
    };
    let mut v: Vec<_> = rd
        .flatten()
        .map(|e| (e.path().join("input.dwg"), e.path().join("reference.dxf")))
        .filter(|(a, _)| a.is_file())
        .collect();
    v.sort();
    v
}

/// A record as the DWG builder emits it.
fn dwg_rec(kind: &str, pairs: Vec<(i32, String)>) -> Entity {
    let mut v = vec![dxf::Pair::new(0, kind), dxf::Pair::new(8, "L")];
    v.extend(pairs.into_iter().map(|(c, s)| dxf::Pair::new(c, s)));
    Entity::Unknown(v)
}

type Row5 =(String, f64, f64, String, String);

/// Visible content of the drawing tables, comparable between two models.
#[derive(Debug, PartialEq)]
struct Snap {
    counts: BTreeMap<&'static str, i64>,
    blocks: BTreeSet<String>,
    /// Layers that carry at least one entity.
    used_layers: BTreeSet<String>,
    /// Model-space lines as (layer, x1, y1, x2, y2), sorted.
    lines: Vec<(String, f64, f64, f64, f64)>,
    texts: Vec<String>,
    /// (block, insert x, insert y, tag, value), sorted.
    attrs: Vec<Row5>,
}

fn strings(m: &Model, sql: &str) -> Vec<String> {
    let mut st = m.sql().prepare(sql).unwrap();
    let v = st
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    v
}

fn snap(m: &Model) -> Snap {
    let counts = TABLES
        .iter()
        .map(|t| {
            let n: i64 = m
                .sql()
                .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
                .unwrap();
            (*t, n)
        })
        .collect();
    let mut used = BTreeSet::new();
    for t in ["d_line", "d_polyline", "d_arc", "d_text", "d_insert"] {
        used.extend(strings(
            m,
            &format!("SELECT DISTINCT l.name FROM {t} e JOIN d_layer l ON l.id=e.layer"),
        ));
    }
    let mut lines: Vec<(String, f64, f64, f64, f64)> = {
        let mut st = m
            .sql()
            .prepare(
                "SELECT l.name,x1,y1,x2,y2 FROM d_line e JOIN d_layer l ON l.id=e.layer \
                 WHERE e.block IS NULL",
            )
            .unwrap();
        let v = st
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v
    };
    lines.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut texts = strings(m, "SELECT content FROM d_text");
    texts.sort();
    let mut attrs: Vec<Row5> = {
        let mut st = m
            .sql()
            .prepare(
                "SELECT b.name,i.x,i.y,a.tag,a.value FROM d_attr a \
                 JOIN d_insert i ON i.id=a.insert_id JOIN d_block b ON b.id=i.def",
            )
            .unwrap();
        let v = st
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v
    };
    attrs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Snap {
        counts,
        blocks: strings(m, "SELECT name FROM d_block").into_iter().collect(),
        used_layers: used,
        lines,
        texts,
        attrs,
    }
}

/// Same drawing: counts, names and text equal, coordinates within 1e-6.
fn same(a: &Snap, b: &Snap) {
    assert_eq!(a.counts, b.counts);
    assert_eq!(a.blocks, b.blocks);
    assert_eq!(a.used_layers, b.used_layers);
    assert_eq!(a.texts, b.texts);
    assert_eq!(a.lines.len(), b.lines.len());
    for (x, y) in a.lines.iter().zip(&b.lines) {
        assert_eq!(x.0, y.0);
        for (p, q) in [(x.1, y.1), (x.2, y.2), (x.3, y.3), (x.4, y.4)] {
            assert!((p - q).abs() < 1e-6, "{x:?} vs {y:?}");
        }
    }
    assert_eq!(a.attrs.len(), b.attrs.len());
    for (x, y) in a.attrs.iter().zip(&b.attrs) {
        assert_eq!((&x.0, &x.3, &x.4), (&y.0, &y.3, &y.4));
        assert!((x.1 - y.1).abs() < 1e-6 && (x.2 - y.2).abs() < 1e-6, "{x:?} vs {y:?}");
    }
}

fn line(layer: &str, a: (f64, f64), b: (f64, f64)) -> Entity {
    Entity::Line(dxf::Line {
        common: Common::on_layer(layer),
        start: [a.0, a.1, 0.0],
        end: [b.0, b.1, 0.0],
    })
}

fn text(layer: &str, s: &str) -> Entity {
    Entity::Text(dxf::Text {
        common: Common::on_layer(layer),
        pos: [1.0, 2.0, 0.0],
        height: 0.25,
        text: s.into(),
        rotation: 0.0,
        style: None,
    })
}

fn insert(x: f64, y: f64, no: &str) -> Entity {
    Entity::Insert(Insert {
        common: Common::on_layer("DOORS"),
        block: "DOOR".into(),
        pos: [x, y, 0.0],
        attribs: vec![Attrib {
            common: Common::on_layer("DOORS"),
            tag: "NUM".into(),
            value: no.into(),
            pos: [x, y, 0.0],
            height: 0.1,
            flags: 0,
        }],
        ..Default::default()
    })
}

/// German, Chinese and Russian text written with escapes (no literal non-ASCII in the source).
const UNI: &str = "Gr\u{fc}\u{df}e \u{4e2d}\u{6587} \u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}";

/// A drawing with every supported kind, as the DWG builder would hand it over.
fn sample() -> dxf::DxfDocument {
    let mut door = Block::new("DOOR");
    door.entities.push(line("0", (0.0, 0.0), (0.9, 0.0)));
    door.attdefs.push(AttDef {
        common: Common::on_layer("DOORS"),
        tag: "NUM".into(),
        prompt: "Number".into(),
        default: "?".into(),
        pos: [0.0, 0.1, 0.0],
        height: 0.1,
        flags: 0,
    });
    let poly = LwPolyline {
        common: Common::on_layer("WALLS"),
        flags: 1,
        vertices: [(0.0, 0.0, 0.0), (4.0, 0.0, 0.5), (4.0, 3.0, 0.0), (0.0, 3.0, 0.0)]
            .map(|(x, y, bulge)| LwVertex {
                x,
                y,
                bulge,
                ..Default::default()
            })
            .to_vec(),
    };
    let mut spline = vec![
        (70, "1024".to_string()),
        (71, "3".into()),
        (72, "0".into()),
        (73, "0".into()),
        (74, "3".into()),
    ];
    for (x, y) in [(0, 0), (2, 1), (4, 0)] {
        spline.extend([(11, x.to_string()), (21, y.to_string()), (31, "0".into())]);
    }
    dxf::DxfDocument {
        blocks: vec![door],
        entities: vec![
            line("WALLS", (0.0, 0.0), (4.0, 0.0)),
            line("WALLS", (4.0, 0.0), (4.0, 3.0)),
            line("AXES", (-1.0, 1.5), (5.0, 1.5)),
            Entity::LwPolyline(poly),
            Entity::Arc(dxf::Arc {
                common: Common::on_layer("DOORS"),
                center: [1.0, 1.0, 0.0],
                radius: 0.9,
                start_angle: 0.0,
                end_angle: 90.0,
            }),
            text("NOTES", "Hall"),
            text("NOTES", UNI),
            insert(1.0, 0.0, "D-1"),
            insert(3.0, 0.0, "D-\u{fc}2"),
            dwg_rec("SPLINE", spline),
            dwg_rec("DIMENSION", vec![]),
        ],
        ..Default::default()
    }
}

fn import(doc: &dxf::DxfDocument) -> (Model, dxf::ImportReport) {
    let mut m = Model::open(":memory:").unwrap();
    let rep = dxf::import_into(&mut m, doc).unwrap();
    (m, rep)
}

/// Model -> DXF text -> model.
fn reload(m: &Model) -> (String, Model, dxf::ImportReport) {
    let text = dxf::write_string(&dxf::export_from(m).unwrap());
    let doc = dxf::read_str(&text).unwrap();
    let (m2, rep) = import(&doc);
    (text, m2, rep)
}

fn exec(m: &Model, sql: &str) {
    m.sql().execute_batch(sql).unwrap();
}

#[test]
fn dwg_edit_roundtrip_counts() {
    let (m, rep) = import(&sample());
    // Lines: three in model space plus one in the block definition.
    // the spline is lowered to a second polyline
    assert_eq!((rep.polylines, rep.arcs, rep.curves), (2, 1, 1));
    assert_eq!(rep.dropped.get("dimension"), Some(&1));
    let before = snap(&m);

    // Move a line, change another one's layer, edit a text, delete the arc.
    exec(
        &m,
        "UPDATE d_line SET x1=x1+5, x2=x2+5 WHERE block IS NULL AND y1=0 AND x1=0;
         UPDATE d_line SET layer=(SELECT id FROM d_layer WHERE name='AXES')
           WHERE block IS NULL AND x1=4 AND y1=0;
         UPDATE d_text SET content='Lobby' WHERE content='Hall';
         DELETE FROM d_arc;",
    );
    let edited = snap(&m);
    assert_eq!(edited.counts["d_arc"], before.counts["d_arc"] - 1);
    assert!(edited.texts.contains(&"Lobby".to_string()));

    let (_, m2, rep2) = reload(&m);
    assert!(rep2.dropped.is_empty(), "{:?}", rep2.dropped);
    let again = snap(&m2);
    same(&edited, &again);
    assert!(again
        .lines
        .iter()
        .any(|l| l.0 == "WALLS" && (l.1 - 5.0).abs() < 1e-6 && (l.3 - 9.0).abs() < 1e-6));
    assert!(again.lines.iter().any(|l| l.0 == "AXES" && (l.1 - 4.0).abs() < 1e-6));
    assert_eq!(again.counts["d_polyline"], before.counts["d_polyline"]);
}

#[test]
fn dwg_roundtrip_keeps_layers_blocks_attribs() {
    let (m, _) = import(&sample());
    // Move one block insert; its attribute value stays with it.
    exec(&m, "UPDATE d_insert SET x=x+2.5, y=y+1 WHERE x=3");
    let edited = snap(&m);
    assert!(edited.blocks.contains("DOOR"));
    assert_eq!(edited.attrs.len(), 2);

    let (_, m2, rep2) = reload(&m);
    let again = snap(&m2);
    same(&edited, &again);
    for l in ["WALLS", "AXES", "DOORS", "NOTES"] {
        assert!(again.used_layers.contains(l), "{l}");
    }
    assert_eq!((rep2.inserts, rep2.attributes), (2, 2));
    let moved: Vec<_> = again.attrs.iter().filter(|a| a.4 == "D-\u{fc}2").collect();
    assert_eq!(moved.len(), 1);
    assert!((moved[0].1 - 5.5).abs() < 1e-6 && (moved[0].2 - 1.0).abs() < 1e-6);
    let def: String = m2
        .sql()
        .query_row(
            "SELECT default_value FROM d_attdef a JOIN d_block b ON b.id=a.block
             WHERE b.name='DOOR' AND a.tag='NUM'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(def, "?");
}

#[test]
fn dwg_roundtrip_unicode_text() {
    let (m, _) = import(&sample());
    let edited_text = format!("{UNI} 2");
    exec(
        &m,
        &format!("UPDATE d_text SET content='{edited_text}' WHERE content='{UNI}'"),
    );
    let (text, m2, _) = reload(&m);
    let got = snap(&m2).texts;
    assert!(got.contains(&edited_text), "{got:?}");
    assert!(got.contains(&"Hall".to_string()));
    assert!(!text.contains("\\U+"), "escapes must decode on reading back");
}

#[test]
fn dwg_second_roundtrip_identical() {
    let (m, rep) = import(&sample());
    exec(
        &m,
        "UPDATE d_line SET x2=x2+1
         WHERE block IS NULL AND layer=(SELECT id FROM d_layer WHERE name='AXES')",
    );
    let (t1, m2, rep2) = reload(&m);
    let (t2, m3, _) = reload(&m2);
    assert_eq!(t1, t2, "second DXF differs from the first");
    same(&snap(&m2), &snap(&m3));
    // The only kind lost is the one the first import reported; the file has no trace of it.
    assert_eq!(rep.dropped.len(), 1);
    assert_eq!(rep.dropped.get("dimension"), Some(&1));
    assert!(rep2.dropped.is_empty(), "{:?}", rep2.dropped);
    let exported = dxf::read_str(&t1).unwrap();
    assert!(!exported
        .entities
        .iter()
        .any(|e| matches!(e, Entity::Unknown(v) if v[0].value == "DIMENSION")));
}

/// Every corpus drawing: edit in the model, write DXF, import again, compare. Set
/// `DDC_DWG_REQUIRE=1` to demand at least 30 files.
#[test]
#[ignore = "local corpus: DDC_DWG_CORPUS"]
fn corpus_edit_roundtrip() {
    let files = corpus();
    let required = std::env::var("DDC_DWG_REQUIRE").is_ok();
    if files.is_empty() {
        eprintln!("dwg corpus absent; skipped");
        assert!(!required, "corpus required but absent");
        return;
    }
    if required {
        assert!(files.len() >= 30, "only {} corpus files", files.len());
    }
    let (mut ok, mut skipped, mut bad) = (0, 0, vec![]);
    for (path, _) in &files {
        let Ok(doc) = read_file(path) else {
            skipped += 1;
            continue;
        };
        let (m, _) = import(&doc);
        exec(
            &m,
            &format!(
                "UPDATE d_line SET x1=x1+5, x2=x2+5
                   WHERE id=(SELECT min(id) FROM d_line WHERE block IS NULL);
                 UPDATE d_text SET content='{UNI}'
                   WHERE id=(SELECT min(id) FROM d_text WHERE block IS NULL);
                 DELETE FROM d_arc WHERE id=(SELECT min(id) FROM d_arc WHERE block IS NULL);"
            ),
        );
        let edited = snap(&m);
        let (t1, m2, rep2) = reload(&m);
        let again = snap(&m2);
        let mut errs = vec![];
        if edited.counts != again.counts {
            errs.push(format!("counts {:?} vs {:?}", edited.counts, again.counts));
        }
        if edited.blocks != again.blocks {
            errs.push("blocks".to_string());
        }
        if edited.used_layers != again.used_layers {
            errs.push("layers".to_string());
        }
        if edited.texts != again.texts {
            errs.push("texts".to_string());
        }
        if edited.lines.len() != again.lines.len()
            || edited
                .lines
                .iter()
                .zip(&again.lines)
                .any(|(a, b)| a.0 != b.0 || (a.1 - b.1).abs() > 1e-6 || (a.4 - b.4).abs() > 1e-6)
        {
            errs.push("lines".to_string());
        }
        let key = |s: &Snap| -> Vec<(String, String, String)> {
            s.attrs
                .iter()
                .map(|a| (a.0.clone(), a.3.clone(), a.4.clone()))
                .collect()
        };
        if key(&edited) != key(&again) {
            errs.push("attribs".to_string());
        }
        if !rep2.dropped.is_empty() {
            errs.push(format!("second import dropped {:?}", rep2.dropped));
        }
        if t1 != reload(&m2).0 {
            errs.push("second round trip differs".to_string());
        }
        if errs.is_empty() {
            ok += 1;
        } else {
            bad.push(format!("{}: {}", path.display(), errs.join("; ")));
        }
    }
    eprintln!("edit round trip: {ok} ok, {skipped} unreadable, {} bad", bad.len());
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    assert!(ok > 0);
}
