// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Reference plan for tests and snapshots: two 6×8 m rooms within 300 mm outer walls and a
//! 200 mm partition, door and window blocks, axes with tags, columns with a solid fill,
//! an area outline (legacy POLYLINE), MTEXT with codes, a dimension with an anonymous block, pattern
//! hatching, an ellipse and a spline, a sheet with a caption. Units are mm (`$INSUNITS` = 4).

use crate::dxf::lower::hatch_record;
use crate::ru_fixtures::ru;
use crate::dxf::{
    self, Arc, Block, Circle, Common, DxfDocument, Entity, HeaderVar, Insert, Layer, Line,
    LineType, MText, Pair, RawSection, Text,
};

const K: f64 = 1000.0;

fn mm(p: (f64, f64)) -> [f64; 3] {
    [p.0 * K, p.1 * K, 0.0]
}

fn line(layer: &str, a: (f64, f64), b: (f64, f64)) -> Entity {
    Entity::Line(Line {
        common: Common::on_layer(layer),
        start: mm(a),
        end: mm(b),
    })
}

fn raw(kind: &str, pairs: &[(i32, String)]) -> Entity {
    let mut v = vec![Pair::new(0, kind)];
    v.extend(pairs.iter().map(|(c, s)| Pair::new(*c, s.clone())));
    Entity::Unknown(v)
}

fn text(layer: &str, p: (f64, f64), h: f64, s: &str) -> Entity {
    Entity::Text(Text {
        common: Common::on_layer(layer),
        pos: mm(p),
        height: h * K,
        text: s.into(),
        rotation: 0.0,
        style: None,
    })
}

fn insert(block: &str, p: (f64, f64), rot: f64, layer: &str) -> Entity {
    Entity::Insert(Insert {
        common: Common::on_layer(layer),
        block: block.into(),
        pos: mm(p),
        rotation: rot,
        ..Default::default()
    })
}

/// A wall as two faces with gaps `gaps` (from the axis start, m) and end caps at the gaps.
fn wall(
    out: &mut Vec<Entity>,
    a: (f64, f64),
    b: (f64, f64),
    t: f64,
    ext: (f64, f64),
    gaps: &[(f64, f64)],
) {
    let len = (b.0 - a.0).hypot(b.1 - a.1);
    let d = ((b.0 - a.0) / len, (b.1 - a.1) / len);
    let n = (-d.1, d.0);
    let at = |s: f64, side: f64| (a.0 + d.0 * s + n.0 * side, a.1 + d.1 * s + n.1 * side);
    for (side, (e0, e1)) in [(-t / 2.0, (ext.0, ext.1)), (t / 2.0, (-ext.0, -ext.1))] {
        let mut s = -e0;
        for &(g0, g1) in gaps {
            out.push(line("A-WALL", at(s, side), at(g0, side)));
            s = g1;
        }
        out.push(line("A-WALL", at(s, side), at(len + e1, side)));
    }
    for &(g0, g1) in gaps {
        out.push(line("A-WALL", at(g0, -t / 2.0), at(g0, t / 2.0)));
        out.push(line("A-WALL", at(g1, -t / 2.0), at(g1, t / 2.0)));
    }
}

fn layer(name: &str, color: i32, lt: &str, lw: Option<i32>) -> Layer {
    Layer {
        name: name.into(),
        flags: 0,
        color,
        linetype: lt.into(),
        extra: lw.map_or(vec![], |w| vec![Pair::new(370, w.to_string())]),
    }
}

fn rect(c: (f64, f64), w: f64, h: f64) -> Vec<(f64, f64)> {
    let (x, y) = (w / 2.0, h / 2.0);
    vec![
        (c.0 - x, c.1 - y),
        (c.0 + x, c.1 - y),
        (c.0 + x, c.1 + y),
        (c.0 - x, c.1 + y),
    ]
}

/// Floor plan; `variant` 1 — the partition is shifted to axis x = 4 m (second floor).
pub fn sample_plan(variant: u8) -> DxfDocument {
    let mut d = DxfDocument::default();
    let hv = |name: &str, code: i32, v: &str| HeaderVar {
        name: name.into(),
        values: vec![Pair::new(code, v)],
    };
    d.header = vec![hv("$INSUNITS", 70, "4"), hv("$LTSCALE", 40, "1.0")];
    d.ltypes.push(LineType {
        name: "CENTER".into(),
        description: "Center ____ _ ____".into(),
        flags: 0,
        pattern_length: 2000.0,
        pattern: vec![1200.0, -300.0, 200.0, -300.0],
        extra: vec![],
    });
    d.ltypes.push(LineType {
        name: "HIDDEN".into(),
        description: "Hidden __ __".into(),
        flags: 0,
        pattern_length: 300.0,
        pattern: vec![200.0, -100.0],
        extra: vec![],
    });
    d.layers = vec![
        layer("0", 7, "CONTINUOUS", None),
        layer("A-WALL", 7, "CONTINUOUS", Some(50)),
        layer("A-DOOR", 3, "CONTINUOUS", Some(25)),
        layer("A-WIND", 4, "CONTINUOUS", Some(25)),
        layer("S-GRID", 1, "CENTER", Some(18)),
        layer("S-COLUMN", 8, "CONTINUOUS", Some(35)),
        layer("A-AREA", 6, "HIDDEN", Some(13)),
        layer("A-ANNO-TEXT", 7, "CONTINUOUS", None),
        layer("A-DIMS", 140, "CONTINUOUS", Some(13)),
        layer("A-HATCH", 253, "CONTINUOUS", Some(9)),
        layer("A-FURN", 30, "CONTINUOUS", Some(18)),
    ];

    // blocks
    let mut door = Block::new("DOOR_900");
    door.entities = vec![
        line("0", (0.0, 0.0), (0.0, 0.9)),
        Entity::Arc(Arc {
            common: Common::on_layer("0"),
            center: [0.0; 3],
            radius: 900.0,
            start_angle: 0.0,
            end_angle: 90.0,
        }),
    ];
    let mut win = Block::new("WIN_1500");
    win.entities = vec![
        line("0", (0.0, -0.04), (1.5, -0.04)),
        line("0", (0.0, 0.04), (1.5, 0.04)),
    ];
    let mut dim = Block::new("*D1");
    dim.flags = 1;
    dim.entities = vec![
        line("0", (0.0, -1.0), (12.0, -1.0)),
        line("0", (0.0, -0.15), (0.0, -1.15)),
        line("0", (12.0, -0.15), (12.0, -1.15)),
        Entity::MText(MText {
            common: Common::on_layer("0"),
            pos: mm((5.6, -0.85)),
            height: 250.0,
            text: "12000".into(),
            ..Default::default()
        }),
    ];
    d.blocks = vec![door, win, dim];

    let mut e: Vec<Entity> = vec![];
    let xi = if variant == 1 { 4.0 } else { 6.0 };
    // outer walls on axes 0..12 × 0..8, 300 mm; corners continue the faces
    let ext = (0.15, 0.15);
    wall(&mut e, (0.0, 0.0), (12.0, 0.0), 0.3, ext, &[(2.0, 2.9)]);
    wall(&mut e, (12.0, 0.0), (12.0, 8.0), 0.3, ext, &[]);
    wall(
        &mut e,
        (12.0, 8.0),
        (0.0, 8.0),
        0.3,
        ext,
        &[(2.5, 4.0), (8.5, 10.0)],
    );
    wall(&mut e, (0.0, 8.0), (0.0, 0.0), 0.3, ext, &[]);
    // 200 mm partition from the outer wall faces
    wall(
        &mut e,
        (xi, 0.15),
        (xi, 7.85),
        0.2,
        (0.0, 0.0),
        &[(2.85, 3.75)],
    );
    e.push(insert("DOOR_900", (2.0, 0.0), 0.0, "A-DOOR"));
    e.push(insert("DOOR_900", (xi, 3.0), 90.0, "A-DOOR"));
    e.push(insert("WIN_1500", (2.0, 8.0), 0.0, "A-WIND"));
    e.push(insert("WIN_1500", (8.0, 8.0), 0.0, "A-WIND"));

    // axes with tags: vertical 1..3, horizontal A..V (Cyrillic letters)
    let r = 0.4;
    for (k, x) in [0.0, xi, 12.0].iter().enumerate() {
        e.push(line("S-GRID", (*x, -1.6), (*x, 9.6)));
        let c = (*x, 9.6 + r);
        e.push(Entity::Circle(Circle {
            common: Common::on_layer("S-GRID"),
            center: mm(c),
            radius: r * K,
        }));
        e.push(text(
            "S-GRID",
            (c.0 - 0.1, c.1 - 0.15),
            0.3,
            &(k + 1).to_string(),
        ));
    }
    for (k, y) in [0.0, 4.0, 8.0].iter().enumerate() {
        e.push(line("S-GRID", (-1.6, *y), (13.6, *y)));
        let c = (13.6 + r, *y);
        e.push(Entity::Circle(Circle {
            common: Common::on_layer("S-GRID"),
            center: mm(c),
            radius: r * K,
        }));
        e.push(text(
            "S-GRID",
            (c.0 - 0.1, c.1 - 0.15),
            0.3,
            [ru("grid_a"), ru("grid_b"), ru("grid_v")][k],
        ));
    }

    // columns: a filled rectangle and a round one
    for c in [(3.0, 4.0), (9.0, 4.0)] {
        let ring = rect(c, 0.4, 0.4);
        e.push(Entity::LwPolyline(dxf::LwPolyline {
            common: Common::on_layer("S-COLUMN"),
            flags: 1,
            vertices: ring
                .iter()
                .map(|p| dxf::LwVertex {
                    x: p.0 * K,
                    y: p.1 * K,
                    ..Default::default()
                })
                .collect(),
        }));
        let mm_ring: Vec<(f64, f64)> = ring.iter().map(|p| (p.0 * K, p.1 * K)).collect();
        e.push(Entity::Unknown(hatch_record(
            &Common::on_layer("S-COLUMN"),
            &[mm_ring],
            "SOLID",
        )));
    }
    e.push(Entity::Circle(Circle {
        common: Common::on_layer("S-COLUMN"),
        center: mm((9.0, 6.0)),
        radius: 200.0,
    }));

    // area outline of the right room: legacy POLYLINE + VERTEX + SEQEND
    let (x0, x1) = (xi + 0.1, 11.85);
    let s = |v: f64| format!("{}", v * K);
    e.push(raw(
        "POLYLINE",
        &[
            (8, "A-AREA".into()),
            (66, "1".into()),
            (10, "0".into()),
            (20, "0".into()),
            (30, "0".into()),
            (70, "1".into()),
        ],
    ));
    for p in [(x0, 0.15), (x1, 0.15), (x1, 7.85), (x0, 7.85)] {
        e.push(raw(
            "VERTEX",
            &[
                (8, "A-AREA".into()),
                (10, s(p.0)),
                (20, s(p.1)),
                (30, "0".into()),
            ],
        ));
    }
    e.push(raw("SEQEND", &[(8, "A-AREA".into())]));

    // labels: MTEXT with \U+ and formatting, TEXT with %% codes
    let lc = (xi / 2.0, 5.2);
    e.push(Entity::MText(MText {
        common: Common::on_layer("A-ANNO-TEXT"),
        pos: mm((lc.0 - 0.6, lc.1)),
        height: 300.0,
        text: format!("\\U+041A\\U+0443{}", ru("xnya")),
        ..Default::default()
    }));
    e.push(text("A-ANNO-TEXT", (lc.0 - 0.3, lc.1 - 0.5), 0.25, "101"));
    let rc = ((xi + 12.0) / 2.0, 5.2);
    e.push(Entity::MText(MText {
        common: Common::on_layer("A-ANNO-TEXT"),
        pos: mm((rc.0 - 0.8, rc.1)),
        height: 300.0,
        text: format!("{{\\fArial|b1;{}}}", ru("living_room")),
        ..Default::default()
    }));
    e.push(text("A-ANNO-TEXT", (rc.0 - 0.3, rc.1 - 0.5), 0.25, "102"));
    e.push(text("A-DIMS", (9.3, 6.3), 0.18, "%%c400"));

    // a dimension via an anonymous block
    e.push(raw(
        "DIMENSION",
        &[
            (8, "A-DIMS".into()),
            (2, "*D1".into()),
            (10, "0".into()),
            (20, "-1000".into()),
            (70, "32".into()),
        ],
    ));

    // pattern hatching at 45° with 200 mm spacing in the left room
    let hl = [(0.6, 6.0), (2.4, 6.0), (2.4, 7.6), (0.6, 7.6)];
    let mut h = vec![
        (8, "A-HATCH".to_string()),
        (10, "0".into()),
        (20, "0".into()),
        (30, "0".into()),
        (2, "ANSI31".into()),
        (70, "0".into()),
        (71, "0".into()),
        (91, "1".into()),
        (92, "3".into()),
        (72, "0".into()),
        (73, "1".into()),
        (93, "4".into()),
    ];
    for p in hl {
        h.push((10, s(p.0)));
        h.push((20, s(p.1)));
    }
    h.extend([
        (97, "0".into()),
        (75, "1".into()),
        (76, "1".into()),
        (52, "0".into()),
        (41, "1".into()),
        (77, "0".into()),
        (78, "1".into()),
        (53, "45".into()),
        (43, "0".into()),
        (44, "0".into()),
        (45, "-141.4213562373095".into()),
        (46, "141.4213562373095".into()),
        (79, "0".into()),
        (98, "0".into()),
    ]);
    e.push(raw("HATCH", &h));

    // furniture: an ellipse (sink) and a spline (sofa)
    e.push(raw(
        "ELLIPSE",
        &[
            (8, "A-FURN".into()),
            (10, s(1.2)),
            (20, s(1.2)),
            (30, "0".into()),
            (11, s(0.35)),
            (21, "0".into()),
            (31, "0".into()),
            (40, "0.6".into()),
            (41, "0".into()),
            (42, format!("{}", std::f64::consts::TAU)),
        ],
    ));
    let mut sp = vec![
        (8, "A-FURN".to_string()),
        (70, "8".into()),
        (71, "3".into()),
        (72, "8".into()),
        (73, "4".into()),
    ];
    for k in [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0] {
        sp.push((40, format!("{k}")));
    }
    for p in [
        (xi + 1.0, 1.0),
        (xi + 1.5, 2.2),
        (xi + 3.0, 2.2),
        (xi + 3.5, 1.0),
    ] {
        sp.extend([(10, s(p.0)), (20, s(p.1)), (30, "0".into())]);
    }
    e.push(raw("SPLINE", &sp));

    // paper space: the frame and caption do not reach the underlay
    e.push(raw(
        "LINE",
        &[
            (67, "1".into()),
            (8, "0".into()),
            (10, "0".into()),
            (20, "0".into()),
            (30, "0".into()),
            (11, "420".into()),
            (21, "0".into()),
            (31, "0".into()),
        ],
    ));
    d.entities = e;
    d.other_sections.push(RawSection {
        name: "OBJECTS".into(),
        pairs: vec![
            Pair::new(0, "LAYOUT"),
            Pair::new(5, "A0"),
            Pair::new(100, "AcDbLayout"),
            Pair::new(1, "Model"),
            Pair::new(0, "LAYOUT"),
            Pair::new(5, "A1"),
            Pair::new(100, "AcDbLayout"),
            Pair::new(1, ru("layout_1")),
        ],
    });
    // via text: both writing and reading are checked (the DXF writer exists only in tests)
    #[cfg(any(test, feature = "dxf-write"))]
    let d = dxf::read_str(&dxf::write_string(&d)).expect("fixture round trip");
    d
}

/// DXF text of the reference plan.
#[cfg(any(test, feature = "dxf-write"))]
pub fn sample_plan_text(variant: u8) -> String {
    dxf::write_string(&sample_plan(variant))
}
