// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Blocks that must not be lost or restyled on DXF export: a moved INSERT with a tilted
//! extrusion, and the layers that block definitions use.

use super::source::Underlay;
use super::write::{to_dxf, Placement};
use crate::dxf::{
    self, Attrib, Block, Common, DxfDocument, Entity, HeaderVar, Insert, Layer, Line, Pair,
};

const UNIT: f64 = 0.001;

fn on(layer: &str) -> Common {
    Common::on_layer(layer)
}

fn layer(name: &str, color: i32, weight: Option<i32>) -> Layer {
    Layer {
        name: name.into(),
        flags: 0,
        color,
        linetype: "CONTINUOUS".into(),
        extra: weight.map(|w| vec![Pair::new(370, w.to_string())]).unwrap_or_default(),
    }
}

/// One block "TB" (a line on BLK-L) inserted once, on a vertical plane (extrusion 0,1,0),
/// with one attribute (mm).
fn tilted_doc() -> DxfDocument {
    let mut d = DxfDocument {
        header: vec![HeaderVar {
            name: "$INSUNITS".into(),
            values: vec![Pair::new(70, "4")],
        }],
        layers: vec![layer("0", 7, None), layer("BLK-L", 3, Some(50)), layer("A-TAG", 5, None)],
        ..Default::default()
    };
    let mut b = Block::new("TB");
    b.entities = vec![Entity::Line(Line {
        common: on("BLK-L"),
        start: [0.0, 0.0, 0.0],
        end: [100.0, 0.0, 0.0],
    })];
    d.blocks = vec![b];
    let mut common = on("0");
    common.extra = vec![
        Pair::new(210, "0.0"),
        Pair::new(220, "1.0"),
        Pair::new(230, "0.0"),
    ];
    d.entities = vec![Entity::Insert(Insert {
        common,
        block: "TB".into(),
        pos: [0.0, 0.0, 0.0],
        scale: [1.0, 1.0, 1.0],
        rotation: 0.0,
        attribs: vec![Attrib {
            common: on("A-TAG"),
            tag: "NO".into(),
            value: "7".into(),
            pos: [30.0, 40.0, 0.0],
            height: 50.0,
            flags: 0,
        }],
    })];
    d
}

fn back(doc: &DxfDocument) -> DxfDocument {
    dxf::read_str(&dxf::write_string(doc)).expect("written text reads back")
}

fn inserts(doc: &DxfDocument) -> Vec<&Insert> {
    doc.entities
        .iter()
        .filter_map(|e| match e {
            Entity::Insert(i) => Some(i),
            _ => None,
        })
        .collect()
}

fn close(a: [f64; 3], x: f64, y: f64) -> bool {
    (a[0] - x).abs() < 1e-6 && (a[1] - y).abs() < 1e-6
}

#[test]
fn moved_tilted_insert_written_as_primitives() {
    let u = Underlay::from_dxf(&tilted_doc(), UNIT);
    assert_eq!(u.blocks.refs.len(), 1);
    // before the move: the INSERT comes back as read, with its attribute
    let (out0, rep0) = to_dxf(&u, &Placement::file(UNIT));
    assert_eq!((rep0.blocks, rep0.blocks_exploded), (1, 0));
    let d0 = back(&out0);
    assert_eq!(inserts(&d0).len(), 1);
    assert_eq!(inserts(&d0)[0].attribs[0].value, "7");
    // after a 2 m move: its contents are plain primitives at the new place
    let mut m = u.clone();
    m.transform((0.0, 0.0), 0.0, (2.0, 0.0));
    let (out, rep) = to_dxf(&m, &Placement::file(UNIT));
    let d = back(&out);
    assert!(inserts(&d).is_empty());
    let lines: Vec<_> = d
        .entities
        .iter()
        .filter_map(|e| match e {
            Entity::Line(l) if l.common.layer == "BLK-L" => Some(l),
            _ => None,
        })
        .collect();
    assert_eq!(lines.len(), 1, "the block's line is written");
    assert!(close(lines[0].start, 2000.0, 0.0) && close(lines[0].end, 2100.0, 0.0));
    let texts: Vec<_> = d
        .entities
        .iter()
        .filter_map(|e| match e {
            Entity::Text(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 1, "the attribute value is written as a plain text");
    assert_eq!(texts[0].text, "7");
    assert!(close(texts[0].pos, 2030.0, 40.0));
    assert_eq!((rep.lines, rep.texts), (1, 1));
}

#[test]
fn tilted_insert_counted_exploded_not_skipped() {
    let u = Underlay::from_dxf(&tilted_doc(), UNIT);
    let mut m = u.clone();
    m.transform((0.0, 0.0), 0.0, (0.5, 0.25));
    let (_, rep) = to_dxf(&m, &Placement::file(UNIT));
    assert_eq!(rep.blocks, 0);
    assert_eq!(rep.blocks_exploded, 1);
    assert_eq!(rep.skipped, 0);
    // turned about a pivot: the same
    let mut t = u.clone();
    t.transform((1.0, 1.0), 0.5, (0.0, 0.0));
    let (out, rep) = to_dxf(&t, &Placement::file(UNIT));
    assert_eq!((rep.blocks, rep.blocks_exploded, rep.skipped), (0, 1, 0));
    assert!(back(&out).entities.iter().any(|e| matches!(e, Entity::Text(x) if x.text == "7")));
}

/// A block whose line sits on layer "BLK-ONLY", which the exported underlay no longer lists.
fn block_layer_doc() -> DxfDocument {
    let mut d = tilted_doc();
    d.layers.push(layer("BLK-ONLY", 3, Some(50)));
    d.blocks[0].entities = vec![Entity::Line(Line {
        common: on("BLK-ONLY"),
        start: [0.0, 0.0, 0.0],
        end: [100.0, 0.0, 0.0],
    })];
    if let Entity::Insert(i) = &mut d.entities[0] {
        i.common.extra.clear();
    }
    d
}

fn export_without_layer() -> DxfDocument {
    let mut u = Underlay::from_dxf(&block_layer_doc(), UNIT);
    u.layers.retain(|l| l.name != "BLK-ONLY");
    let (out, rep) = to_dxf(&u, &Placement::file(UNIT));
    assert_eq!((rep.blocks, rep.blocks_exploded), (1, 0));
    back(&out)
}

#[test]
fn block_only_layers_in_layer_table() {
    let d = export_without_layer();
    let b = d.blocks.iter().find(|b| b.name == "TB").expect("definition written");
    for e in &b.entities {
        if let Entity::Line(l) = e {
            assert!(
                d.layers.iter().any(|x| x.name.eq_ignore_ascii_case(&l.common.layer)),
                "layer {} of the block is in the table",
                l.common.layer
            );
        }
    }
    assert!(d.layers.iter().any(|x| x.name == "BLK-ONLY"));
}

#[test]
fn block_layer_keeps_color_weight() {
    let d = export_without_layer();
    let l = d.layers.iter().find(|x| x.name == "BLK-ONLY").expect("layer written");
    assert_eq!(l.color, 3);
    assert_eq!(l.linetype.to_ascii_uppercase(), "CONTINUOUS");
    assert!(l.extra.iter().any(|p| p.code == 370 && p.value.trim() == "50"));
}
