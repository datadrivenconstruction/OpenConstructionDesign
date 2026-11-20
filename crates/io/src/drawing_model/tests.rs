// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::ru_fixtures::ru;
use crate::dxf::{self, Arc as DArc, Common, DxfDocument, Entity, Layer, Line, Text};
use crate::pdf::import::{ImportOptions, PdfFile, Scale};
use crate::pdf::{export_pdf, Item, Sheet, SheetLayer};
use ddc_model::{LayerSpec, Model};

enum Prim {
    Line(&'static str, [f64; 2], [f64; 2]),
    /// Centre, radius, angles in degrees.
    Arc(&'static str, [f64; 2], f64, f64, f64),
    Text(&'static str, [f64; 2], f64, &'static str),
}

const LAYERS: [&str; 7] = [
    "A-WALL", "A-DOOR", "A-TEXT", "A-HATCH", "DIM", "0", "CENTER",
];

fn l(layer: &'static str, a: [f64; 2], b: [f64; 2]) -> Prim {
    Prim::Line(layer, a, b)
}

/// A 6x4 room (axis), 0.2 m walls; a 0.9 m door in the bottom wall, a 1.2 m window in the right one.
fn room() -> Vec<Prim> {
    let w = "A-WALL";
    vec![
        l(w, [-0.1, -0.1], [2.0, -0.1]),
        l(w, [2.9, -0.1], [6.1, -0.1]),
        l(w, [0.1, 0.1], [2.0, 0.1]),
        l(w, [2.9, 0.1], [5.9, 0.1]),
        l(w, [2.0, -0.1], [2.0, 0.1]),
        l(w, [2.9, -0.1], [2.9, 0.1]),
        l(w, [-0.1, -0.1], [-0.1, 4.1]),
        l(w, [0.1, 0.1], [0.1, 3.9]),
        l(w, [-0.1, 4.1], [6.1, 4.1]),
        l(w, [0.1, 3.9], [5.9, 3.9]),
        l(w, [6.1, -0.1], [6.1, 1.5]),
        l(w, [6.1, 2.7], [6.1, 4.1]),
        l(w, [5.9, 0.1], [5.9, 1.5]),
        l(w, [5.9, 2.7], [5.9, 3.9]),
        l(w, [5.9, 1.5], [6.1, 1.5]),
        l(w, [5.9, 2.7], [6.1, 2.7]),
        l(w, [5.97, 1.5], [5.97, 2.7]),
        l(w, [6.03, 1.5], [6.03, 2.7]),
        Prim::Arc("A-DOOR", [2.0, 0.1], 0.9, 0.0, 90.0),
        l("A-DOOR", [2.0, 0.1], [2.0, 1.0]),
        Prim::Text("A-TEXT", [2.2, 2.0], 0.25, ru("kitchen")),
        Prim::Text("A-TEXT", [2.6, 1.6], 0.25, "101"),
    ]
}

fn noise() -> Vec<Prim> {
    let mut v = vec![];
    // Hatching on a layer without a telltale name: 12 lines at 45°, 0.15 m spacing.
    let u = std::f64::consts::FRAC_1_SQRT_2;
    for k in 0..12 {
        let c = 0.15 * k as f64;
        let p = [3.0 - u * c, 0.5 + u * c];
        v.push(l("0", p, [p[0] + 1.2 * u, p[1] + 1.2 * u]));
    }
    for k in 0..12 {
        let y = 0.6 + 0.2 * k as f64;
        v.push(l("A-HATCH", [3.5, y], [5.5, y]));
    }
    v.push(l("DIM", [0.0, -0.8], [6.0, -0.8]));
    v.push(l("DIM", [0.0, -0.9], [0.0, -0.7]));
    v.push(l("0", [-0.1, -1.0], [6.1, -1.0]));
    v.push(l("0", [-0.1, -0.15], [-0.1, -1.1]));
    v.push(l("0", [6.1, -0.15], [6.1, -1.1]));
    v.push(l("CENTER", [3.0, -0.5], [3.0, 4.5]));
    v.push(Prim::Text("DIM", [2.5, -0.75], 0.2, "6000"));
    v
}

fn to_dxf(prims: &[Prim], origin: [f64; 2]) -> DxfDocument {
    let mut d = DxfDocument::default();
    for n in LAYERS {
        d.layers.push(Layer {
            name: n.into(),
            flags: 0,
            color: 7,
            linetype: if n == "CENTER" {
                "DASHED"
            } else {
                "CONTINUOUS"
            }
            .into(),
            extra: vec![],
        });
    }
    let mm = |p: [f64; 2]| {
        [
            (p[0] + origin[0]) * 1000.0,
            (p[1] + origin[1]) * 1000.0,
            0.0,
        ]
    };
    for p in prims {
        d.entities.push(match p {
            Prim::Line(ly, a, b) => Entity::Line(Line {
                common: Common::on_layer(ly),
                start: mm(*a),
                end: mm(*b),
            }),
            Prim::Arc(ly, c, r, a0, a1) => Entity::Arc(DArc {
                common: Common::on_layer(ly),
                center: mm(*c),
                radius: r * 1000.0,
                start_angle: *a0,
                end_angle: *a1,
            }),
            Prim::Text(ly, p, h, s) => Entity::Text(Text {
                common: Common::on_layer(ly),
                pos: mm(*p),
                height: h * 1000.0,
                text: (*s).into(),
                rotation: 0.0,
                style: None,
            }),
        });
    }
    // Via DXF text: both writing and reading are checked.
    dxf::read_str(&dxf::write_string(&d)).unwrap()
}

/// A 1:100 sheet: 1 m = 10 mm. The arc is a polyline of 16 segments.
fn to_pdf(prims: &[Prim], origin: [f64; 2]) -> Vec<u8> {
    let k = 10.0;
    let pt = |p: [f64; 2]| [(p[0] + origin[0]) * k, (p[1] + origin[1]) * k];
    let idx = |n: &str| LAYERS.iter().position(|x| *x == n).unwrap();
    let items = prims
        .iter()
        .map(|p| match p {
            Prim::Line(ly, a, b) => Item::Polyline {
                pts: vec![pt(*a), pt(*b)],
                closed: false,
                weight_mm: 0.25,
                rgb: [0, 0, 0],
                layer: idx(ly),
            },
            Prim::Arc(ly, c, r, a0, a1) => {
                let pts = (0..=16)
                    .map(|i| {
                        let t = (a0 + (a1 - a0) * i as f64 / 16.0).to_radians();
                        pt([c[0] + r * t.cos(), c[1] + r * t.sin()])
                    })
                    .collect();
                Item::Polyline {
                    pts,
                    closed: false,
                    weight_mm: 0.25,
                    rgb: [0, 0, 0],
                    layer: idx(ly),
                }
            }
            Prim::Text(ly, p, h, s) => Item::Text {
                pos: pt(*p),
                height_mm: h * k,
                rotation: 0.0,
                string: (*s).into(),
                rgb: [0, 0, 0],
                layer: idx(ly),
            },
        })
        .collect();
    let sheet = Sheet {
        width_mm: 420.0,
        height_mm: 297.0,
        layers: LAYERS
            .iter()
            .map(|n| SheetLayer {
                name: (*n).into(),
                visible: true,
            })
            .collect(),
        items,
    };
    export_pdf(&[sheet]).unwrap()
}

fn underlay_dxf(prims: &[Prim]) -> Underlay {
    Underlay::from_dxf(&to_dxf(prims, [1.0, 1.0]), 0.001)
}

fn underlay_pdf(prims: &[Prim]) -> Underlay {
    let bytes = to_pdf(prims, [1.0, 1.0]);
    let page = PdfFile::parse(&bytes).unwrap().page(0).unwrap();
    let opt = ImportOptions {
        scale: Scale::Ratio(100.0),
        ..Default::default()
    };
    Underlay::from_pdf(&page, &opt).unwrap()
}

fn types() -> Vec<WallTypeInfo> {
    vec![
        WallTypeInfo {
            id: 1,
            name: "Block 200".into(),
            thickness: 0.2,
        },
        WallTypeInfo {
            id: 2,
            name: "Part 120".into(),
            thickness: 0.12,
        },
    ]
}

fn check_room(p: &Proposals) {
    assert_eq!(p.walls.len(), 4, "{:?}", p.report);
    for w in &p.walls {
        assert!((w.thickness - 0.2).abs() < 2e-3, "{w:?}");
        assert_eq!(w.wall_type, Some(1));
        assert!(w.confidence >= 0.9);
    }
    let doors: Vec<_> = p.openings.iter().filter(|o| o.kind == "door").collect();
    let wins: Vec<_> = p.openings.iter().filter(|o| o.kind == "window").collect();
    assert_eq!((doors.len(), wins.len()), (1, 1), "{:?}", p.openings);
    assert!(doors[0].confidence >= 0.9);
    assert!((doors[0].width - 0.9).abs() < 0.02, "{:?}", doors[0]);
    assert!((wins[0].width - 1.2).abs() < 0.02, "{:?}", wins[0]);
    assert_eq!(p.rooms.len(), 1, "{:?}", p.report);
    assert_eq!(p.rooms[0].name, ru("kitchen"));
    assert_eq!(p.rooms[0].number, "101");
    assert!(
        (p.rooms[0].area - 5.8 * 3.8).abs() < 0.05,
        "{}",
        p.rooms[0].area
    );
    assert_eq!(
        (
            p.report.walls,
            p.report.doors,
            p.report.windows,
            p.report.rooms
        ),
        (4, 1, 1, 1)
    );
}

#[test]
fn room_from_dxf() {
    let u = underlay_dxf(&room());
    let p = propose(&u, &types(), &ProposeOptions::default(), &Memory::default());
    check_room(&p);
    assert!(p.report.unmatched.is_empty(), "{:?}", p.report.unmatched);
    assert_eq!(p.report.door_leaf_lines, 1);
}

#[test]
fn room_from_pdf() {
    let u = underlay_pdf(&room());
    let p = propose(&u, &types(), &ProposeOptions::default(), &Memory::default());
    check_room(&p);
}

#[test]
fn noise_makes_no_false_walls() {
    let mut prims = room();
    prims.extend(noise());
    for u in [underlay_dxf(&prims), underlay_pdf(&prims)] {
        let p = propose(&u, &types(), &ProposeOptions::default(), &Memory::default());
        check_room(&p);
        assert!(p.report.hatch_lines >= 12, "{:?}", p.report);
        assert!(p.report.ignored_layer_lines >= 14, "{:?}", p.report);
        assert!(!p.report.unmatched.is_empty());
    }
    // A dashed centre line is skipped by linetype (only DXF carries the type in the layer).
    let p = propose(
        &underlay_dxf(&prims),
        &[],
        &ProposeOptions::default(),
        &Memory::default(),
    );
    assert!(p.report.dashed_lines >= 1);
    assert!(p.walls.iter().all(|w| w.wall_type.is_none()));
}

#[test]
fn type_ambiguity_is_reported() {
    let mut t = types();
    t[0].thickness = 0.26;
    let p = propose(
        &underlay_dxf(&room()),
        &t,
        &ProposeOptions::default(),
        &Memory::default(),
    );
    assert_eq!(
        p.report
            .ambiguous
            .iter()
            .filter(|a| a.reason.starts_with("wall type"))
            .count(),
        4
    );
    let t2 = vec![
        types()[0].clone(),
        WallTypeInfo {
            id: 3,
            name: "Twin".into(),
            thickness: 0.202,
        },
    ];
    let p = propose(
        &underlay_dxf(&room()),
        &t2,
        &ProposeOptions::default(),
        &Memory::default(),
    );
    assert_eq!(p.report.ambiguous.len(), 4);
}

#[test]
fn deterministic_ids() {
    let u = underlay_dxf(&room());
    let a = propose(&u, &types(), &ProposeOptions::default(), &Memory::default());
    let b = propose(&u, &types(), &ProposeOptions::default(), &Memory::default());
    assert_eq!(a, b);
    let ids = a.all_ids();
    let set: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(set.len(), ids.len());
}

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    for (n, t) in [("Block 200", 0.2), ("Part 120", 0.12)] {
        m.add_wall_type(
            n,
            &[LayerSpec {
                material: "Brick".into(),
                thickness: t,
            }],
        )
        .unwrap();
    }
    (m, lv)
}

fn count(m: &Model, t: &str) -> i64 {
    m.sql()
        .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn accept_is_one_undo_and_memory_sticks() {
    let (mut m, lv) = model();
    let tys = wall_types(&m).unwrap();
    assert_eq!(tys.len(), 2);
    let u = underlay_dxf(&room());
    let opt = ProposeOptions::default();
    let mut mem = Memory::default();
    let p = propose(&u, &tys, &opt, &mem);
    check_room(&p);
    let (n_types, ops0) = (count(&m, "wall_type"), count(&m, "op"));

    // Accept everything except the window; the window is remembered as rejected.
    let win = p.openings.iter().find(|o| o.kind == "window").unwrap().id;
    let ids: Vec<u64> = p.all_ids().into_iter().filter(|i| *i != win).collect();
    let rep = accept_proposals(&mut m, &p, &ids, &AcceptOptions::new(lv), &mut mem).unwrap();
    assert_eq!(
        (rep.walls, rep.openings, rep.rooms, rep.wall_types_created),
        (4, 1, 1, 0)
    );
    assert_eq!(
        (count(&m, "wall"), count(&m, "opening"), count(&m, "room")),
        (4, 1, 1)
    );
    assert_eq!(count(&m, "op") - ops0, 1, "one journal record");
    assert!(m.room_issues().unwrap().is_empty());
    let q = m
        .room_quantities(
            m.sql()
                .query_row("SELECT id FROM room", [], |r| r.get(0))
                .unwrap(),
        )
        .unwrap()
        .unwrap();
    assert!((q.area - 22.04).abs() < 0.1, "{}", q.area);

    // A repeated proposal: nothing new, the window is remembered as rejected.
    let again = propose(&u, &tys, &opt, &mem);
    assert!(again.walls.is_empty() && again.openings.is_empty() && again.rooms.is_empty());
    assert!(mem.rejected.contains(&win));
    assert_eq!(Memory::from_text(&mem.to_text()), mem);

    m.undo().unwrap();
    assert_eq!(
        (count(&m, "wall"), count(&m, "opening"), count(&m, "room")),
        (0, 0, 0)
    );
    assert_eq!(count(&m, "wall_type"), n_types);
}

#[test]
fn opening_implies_wall_and_unknown_ids_reported() {
    let (mut m, lv) = model();
    let u = underlay_dxf(&room());
    let p = propose(&u, &[], &ProposeOptions::default(), &Memory::default());
    let d = p.openings.iter().find(|o| o.kind == "door").unwrap().id;
    let mut mem = Memory::default();
    let rep = accept_proposals(&mut m, &p, &[d, 42], &AcceptOptions::new(lv), &mut mem).unwrap();
    assert_eq!(
        (
            rep.walls,
            rep.implied_walls,
            rep.openings,
            rep.wall_types_created
        ),
        (1, 1, 1, 0)
    );
    assert_eq!(rep.unknown_ids, vec![42]);
    assert_eq!(count(&m, "wall_type"), 2);
    assert!(accept_proposals(&mut m, &p, &[d], &AcceptOptions::new(99), &mut mem).is_err());
}

#[test]
fn empty_and_degenerate_input() {
    let opt = ProposeOptions::default();
    let p = propose(&Underlay::default(), &[], &opt, &Memory::default());
    assert_eq!(p, Proposals::default());
    let mut u = Underlay::default();
    for i in 0..5 {
        let v = if i == 0 { f64::NAN } else { 1.0 };
        u.lines.push(ULine {
            pen: 0,
            layer: "x".into(),
            a: (0.0, 0.0),
            b: (v, v),
            dashed: false,
        });
        u.lines.push(ULine {
            pen: 0,
            layer: "x".into(),
            a: (1.0, 1.0),
            b: (1.0, 1.0),
            dashed: false,
        });
    }
    u.texts.push(UText {
        pen: 0,
        layer: "t".into(),
        pos: (f64::NAN, 0.0),
        height: 1.0,
        rotation: 0.0,
        text: "a".into(),
        fmt: Default::default(),
    });
    let _ = propose(&u, &[], &opt, &Memory::default());
}

/// Budget: ~10,000 lines (a 50x40 room grid + noise), all proposals < 1 s (release).
/// `cargo test --release -p ddc-io drawing_model_budget -- --ignored --nocapture`
#[test]
#[ignore]
fn drawing_model_budget() {
    let mut u = Underlay::default();
    let (nx, ny, c) = (50usize, 40usize, 3.0);
    let mut add = |a: (f64, f64), b: (f64, f64)| {
        u.lines.push(ULine {
            pen: 0,
            layer: "A-WALL".into(),
            a,
            b,
            dashed: false,
        });
    };
    for j in 0..=ny {
        for i in 0..nx {
            let (x0, x1, y) = (i as f64 * c, (i + 1) as f64 * c, j as f64 * c);
            add((x0 - 0.1, y - 0.1), (x1 + 0.1, y - 0.1));
            add((x0 + 0.1, y + 0.1), (x1 - 0.1, y + 0.1));
        }
    }
    for i in 0..=nx {
        for j in 0..ny {
            let (y0, y1, x) = (j as f64 * c, (j + 1) as f64 * c, i as f64 * c);
            add((x - 0.1, y0 - 0.1), (x - 0.1, y1 + 0.1));
            add((x + 0.1, y0 + 0.1), (x + 0.1, y1 - 0.1));
        }
    }
    let n = u.lines.len();
    for k in 0..40 {
        u.texts.push(UText {
            pen: 0,
            layer: "A-TEXT".into(),
            pos: (1.0 + 3.0 * k as f64, 1.0),
            height: 0.25,
            rotation: 0.0,
            text: format!("Room {k}"),
            fmt: Default::default(),
        });
    }
    let t = std::time::Instant::now();
    let p = propose(&u, &types(), &ProposeOptions::default(), &Memory::default());
    let el = t.elapsed();
    println!(
        "{n} lines: {} walls, {} rooms in {el:?}",
        p.walls.len(),
        p.rooms.len()
    );
    assert!(el.as_secs_f64() < 1.0, "{el:?}");
    assert!(p.rooms.len() >= 40);
}

fn rect(layer: &'static str, x: f64, y: f64, w: f64, h: f64) -> Vec<Prim> {
    vec![
        l(layer, [x, y], [x + w, y]),
        l(layer, [x + w, y], [x + w, y + h]),
        l(layer, [x + w, y + h], [x, y + h]),
        l(layer, [x, y + h], [x, y]),
    ]
}

fn assert_room_only(prims: Vec<Prim>) {
    for u in [underlay_dxf(&prims), underlay_pdf(&prims)] {
        let p = propose(&u, &types(), &ProposeOptions::default(), &Memory::default());
        check_room(&p);
    }
}

#[test]
fn furniture_blocks_make_no_walls() {
    let mut prims = room();
    // 1.6x0.8 tables with an inner tabletop outline, on a layer without a telltale name.
    for (x, y) in [(1.0, 2.0), (3.0, 1.0)] {
        prims.extend(rect("0", x, y, 1.6, 0.8));
        prims.extend(rect("0", x + 0.08, y + 0.08, 1.44, 0.64));
    }
    // A table with a narrow cabinet: a 1.6x0.3 rectangle.
    prims.extend(rect("0", 3.0, 3.0, 1.6, 0.3));
    assert_room_only(prims);
}

#[test]
fn dimension_lines_on_plain_layer_make_no_walls() {
    let mut prims = room();
    // A dimension chain on layer "0": two parallel lines with ticks and texts.
    prims.push(l("0", [0.0, -0.8], [6.0, -0.8]));
    prims.push(l("0", [0.0, -0.95], [6.0, -0.95]));
    for x in [0.0, 3.0, 6.0] {
        prims.push(l("0", [x, -1.0], [x, -0.7]));
        prims.push(l("0", [x - 0.05, -0.85], [x + 0.05, -0.75]));
    }
    prims.push(Prim::Text("0", [2.5, -0.75], 0.2, "6000"));
    prims.push(Prim::Text("0", [2.5, -1.2], 0.2, "6000"));
    // A vertical chain on the right.
    prims.push(l("0", [6.9, 0.0], [6.9, 4.0]));
    prims.push(l("0", [7.05, 0.0], [7.05, 4.0]));
    prims.push(Prim::Text("0", [6.95, 2.0], 0.2, "4000"));
    assert_room_only(prims);
}

/// A door in a coordinate system with extrusion (0,0,-1): the centre in x and the angles are mirrored.
#[test]
fn mirrored_door_arc_is_a_door() {
    use crate::dxf::Pair;
    let prims: Vec<Prim> = room()
        .into_iter()
        .filter(|p| !matches!(p, Prim::Arc(..)))
        .collect();
    let mut d = to_dxf(&prims, [1.0, 1.0]);
    let mut common = Common::on_layer("A-DOOR");
    for (code, v) in [(210, "0"), (220, "0"), (230, "-1")] {
        common.extra.push(Pair {
            code,
            value: v.into(),
        });
    }
    // An arc 0..90 about (3,1,1) m in WCS: in OCS the centre is (-3,1), angles 90..180.
    d.entities.push(Entity::Arc(DArc {
        common,
        center: [-3000.0, 1100.0, 0.0],
        radius: 900.0,
        start_angle: 90.0,
        end_angle: 180.0,
    }));
    let d = dxf::read_str(&dxf::write_string(&d)).unwrap();
    let u = Underlay::from_dxf(&d, 0.001);
    assert_eq!(u.arcs.len(), 1);
    let a = &u.arcs[0];
    assert!(
        (a.c.0 - 3.0).abs() < 1e-9 && (a.c.1 - 1.1).abs() < 1e-9,
        "{a:?}"
    );
    assert!(a.a0.abs() < 1e-9 && (a.a1 - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
    let p = propose(&u, &types(), &ProposeOptions::default(), &Memory::default());
    check_room(&p);
}

#[test]
fn unknown_wall_of_opening_is_error_and_rolls_back() {
    let (mut m, lv) = model();
    let u = underlay_dxf(&room());
    let mut p = propose(&u, &types(), &ProposeOptions::default(), &Memory::default());
    let d = p.openings.iter().find(|o| o.kind == "door").unwrap().id;
    p.openings.iter_mut().find(|o| o.id == d).unwrap().wall = 999_999;
    let ids = p.all_ids();
    let r = accept_proposals(
        &mut m,
        &p,
        &ids,
        &AcceptOptions::new(lv),
        &mut Memory::default(),
    );
    assert!(r.is_err());
    assert_eq!(
        (
            count(&m, "wall"),
            count(&m, "opening"),
            count(&m, "wall_type")
        ),
        (0, 0, 2)
    );
}

fn wall_prop(th: f64) -> Proposals {
    let mut p = propose(
        &underlay_dxf(&room()),
        &[],
        &ProposeOptions::default(),
        &Memory::default(),
    );
    p.walls.truncate(1);
    p.walls[0].thickness = th;
    p.walls[0].wall_type = None;
    p.walls[0].type_delta = None;
    p.openings.clear();
    p.rooms.clear();
    p
}

fn add_type(m: &mut Model, name: &str, t: f64) {
    m.add_wall_type(
        name,
        &[LayerSpec {
            material: "X".into(),
            thickness: t,
        }],
    )
    .unwrap();
}

fn accept_one(m: &mut Model, lv: i64, th: f64) -> AcceptReport {
    let mut p = wall_prop(th);
    let dy = 10.0 * count(m, "wall") as f64;
    p.walls[0].a.1 += dy;
    p.walls[0].b.1 += dy;
    let ids = p.all_ids();
    accept_proposals(m, &p, &ids, &AcceptOptions::new(lv), &mut Memory::default()).unwrap()
}

#[test]
fn drawing_type_matches_by_thickness_within_half_mm() {
    let (mut m, lv) = model();
    add_type(&mut m, "Drawing 100 mm", 0.1);
    for th in [0.0996, 0.1004] {
        assert_eq!(accept_one(&mut m, lv, th).wall_types_created, 0);
    }
    assert_eq!(count(&m, "wall_type"), 3);
    // 100.8 mm does not match: a new type is created, the name is taken - a suffix.
    assert_eq!(accept_one(&mut m, lv, 0.1008).wall_types_created, 1);
    let names: Vec<String> = m
        .sql()
        .prepare("SELECT name FROM wall_type ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(names.last().unwrap(), "Drawing 101 mm");
}

#[test]
fn same_named_type_with_other_thickness_is_not_reused() {
    let (mut m, lv) = model();
    add_type(&mut m, "Drawing 100 mm", 0.15);
    let rep = accept_one(&mut m, lv, 0.1);
    assert_eq!(rep.wall_types_created, 1);
    let (name, th): (String, f64) = m
        .sql()
        .query_row(
            "SELECT t.name, l.thickness FROM wall_type t JOIN wall_type_layer l ON l.wtype=t.id              ORDER BY t.id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(name, "Drawing 100 mm (2)");
    assert!((th - 0.1).abs() < 1e-9);
}

#[test]
fn underlay_expands_inserts_and_hints_doors() {
    use crate::dxf::{Block, Common, Entity, Insert, Line};
    let mut d = dxf::DxfDocument::default();
    let mut b = Block::new("DOOR-90");
    b.entities.push(Entity::Line(Line {
        common: Common::on_layer("0"),
        start: [0.0, 0.0, 0.0],
        end: [900.0, 0.0, 0.0],
    }));
    d.blocks.push(b);
    d.entities.push(Entity::Insert(Insert {
        common: Common::on_layer("A-DOOR"),
        block: "DOOR-90".into(),
        pos: [1000.0, 2000.0, 0.0],
        rotation: 90.0,
        ..Default::default()
    }));
    let u = Underlay::from_dxf(&d, 0.001);
    assert_eq!(u.lines.len(), 1);
    let l = &u.lines[0];
    assert_eq!(l.layer, "A-DOOR");
    assert!((l.a.0 - 1.0).abs() < 1e-9 && (l.a.1 - 2.0).abs() < 1e-9);
    assert!(
        (l.b.0 - 1.0).abs() < 1e-9 && (l.b.1 - 2.9).abs() < 1e-9,
        "{l:?}"
    );
    assert_eq!(u.block_hints.len(), 1);
    assert_eq!(u.block_hints[0].kind, crate::dxf::BlockKind::Door);
}

fn near(a: (f64, f64), b: (f64, f64), tol: f64) -> bool {
    (a.0 - b.0).hypot(a.1 - b.1) <= tol
}

#[test]
fn fixture_underlay_has_every_feature() {
    let doc = fixture::sample_plan(0);
    let (u, unit) = Underlay::from_dxf_auto(&doc);
    assert_eq!(unit, 0.001);
    assert_eq!(u.layouts, vec!["Model".to_string(), ru("layout_1").to_string()]);
    assert_eq!(u.paper_skipped, 1);
    let texts: Vec<&str> = u.texts.iter().map(|t| t.text.as_str()).collect();
    for t in [ru("kitchen"), ru("living_room"), "Ø400", "12000", "101", ru("grid_a")] {
        assert!(texts.contains(&t), "{t} in {texts:?}");
    }
    let solid = u.fills.iter().filter(|f| f.solid).count();
    let pat: Vec<_> = u.fills.iter().filter(|f| !f.solid).collect();
    assert_eq!((solid, pat.len()), (2, 1));
    // 45° lines every 0.2 m across a 1.8 × 1.6 m box ((1.8 + 1.6)·sin 45° / 0.2 = 12), all inside
    assert!(pat[0].segs.len() >= 11, "{}", pat[0].segs.len());
    for (a, b) in &pat[0].segs {
        for p in [a, b] {
            assert!(
                p.0 >= 0.6 - 1e-9 && p.0 <= 2.4 + 1e-9 && p.1 >= 6.0 - 1e-9 && p.1 <= 7.6 + 1e-9
            );
        }
    }
    assert_eq!(u.inserts.len(), 4, "named inserts only");
    let d = u.inserts.iter().find(|i| i.name == "DOOR_900").unwrap();
    assert!((d.size.0 - 0.9).abs() < 1e-9 && (d.size.1 - 0.9).abs() < 1e-9);
    // full circles: 6 bubbles + round column
    // a threshold just under a full turn, not TAU itself
    #[allow(clippy::approx_constant)]
    let full = u.arcs.iter().filter(|a| a.a1 - a.a0 > 6.28).count();
    assert_eq!(full, 7);
    // pens: walls 0.5 mm, grid dashed (1.2 m dash), door green
    let wall = u.lines.iter().find(|l| l.layer == "A-WALL").unwrap();
    assert_eq!(u.pen(wall.pen).weight_mm, 0.5);
    let grid = u.lines.iter().find(|l| l.layer == "S-GRID").unwrap();
    assert_eq!(u.pen(grid.pen).dash, vec![1.2f32, -0.3, 0.2, -0.3]);
    let door = u.arcs.iter().find(|a| a.layer == "A-DOOR").unwrap();
    assert_eq!(u.pen(door.pen).color, Some([0, 255, 0]));
    // old POLYLINE → closed outline; ellipse and spline → furniture geometry
    assert!(u
        .closed
        .iter()
        .any(|c| c.layer == "A-AREA" && c.pts.len() == 4));
    assert!(u.lines.iter().filter(|l| l.layer == "A-FURN").count() > 40);
    assert!(u
        .layers
        .iter()
        .any(|l| l.name == "S-GRID" && l.color == Some([255, 0, 0])));
}

#[test]
fn fixture_recognition_is_exact() {
    let doc = fixture::sample_plan(0);
    let (u, _) = Underlay::from_dxf_auto(&doc);
    let p = propose(&u, &types(), &ProposeOptions::default(), &Memory::default());
    // 4 outer 300 mm walls + a 200 mm partition
    assert_eq!(p.walls.len(), 5, "{:?}", p.walls);
    let axes = [
        ((0.0, 0.0), (12.0, 0.0), 0.3),
        ((12.0, 0.0), (12.0, 8.0), 0.3),
        ((0.0, 8.0), (12.0, 8.0), 0.3),
        ((0.0, 0.0), (0.0, 8.0), 0.3),
        ((6.0, 0.15), (6.0, 7.85), 0.2),
    ];
    for (a, b, t) in axes {
        let w = p
            .walls
            .iter()
            .find(|w| {
                (near(w.a, a, 0.16) && near(w.b, b, 0.16))
                    || (near(w.a, b, 0.16) && near(w.b, a, 0.16))
            })
            .unwrap_or_else(|| panic!("wall {a:?}-{b:?} in {:?}", p.walls));
        assert!((w.thickness - t).abs() < 1e-6, "{w:?}");
        // the axis lies exactly on the drawing's axis line
        if (a.0 - b.0).abs() < 1e-9 {
            assert!(
                (w.a.0 - a.0).abs() < 1e-6 && (w.b.0 - a.0).abs() < 1e-6,
                "{w:?}"
            );
        } else {
            assert!(
                (w.a.1 - a.1).abs() < 1e-6 && (w.b.1 - a.1).abs() < 1e-6,
                "{w:?}"
            );
        }
    }
    let center = |o: &OpeningCand| {
        let w = p.walls.iter().find(|w| w.id == o.wall).unwrap();
        let len = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1);
        let t = o.along + o.width / 2.0;
        (
            w.a.0 + (w.b.0 - w.a.0) * t / len,
            w.a.1 + (w.b.1 - w.a.1) * t / len,
        )
    };
    let want = [
        ("door", (2.45, 0.0), 0.9, ru("door_word")),
        ("door", (6.0, 3.45), 0.9, ru("door_word")),
        ("window", (2.75, 8.0), 1.5, ru("window_word")),
        ("window", (8.75, 8.0), 1.5, ru("window_word")),
    ];
    assert_eq!(p.openings.len(), 4, "{:?}", p.openings);
    for (kind, c, w, fam) in want {
        let o = p
            .openings
            .iter()
            .find(|o| near(center(o), c, 1e-3))
            .unwrap_or_else(|| panic!("{kind} at {c:?}: {:?}", p.openings));
        assert_eq!((o.kind.as_str(), o.family.as_str()), (kind, fam));
        assert!((o.width - w).abs() < 1e-6, "{o:?}");
        assert!(o.confidence >= 0.9);
    }
    assert_eq!(p.rooms.len(), 2, "{:?}", p.rooms);
    let names: Vec<&str> = p.rooms.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&ru("kitchen")) && names.contains(&ru("living_room")),
        "{names:?}"
    );
    let living = p.rooms.iter().find(|r| r.name == ru("living_room")).unwrap();
    assert_eq!(living.boundary.len(), 4);
    let mut g: Vec<&str> = p.grids.iter().map(|g| g.name.as_str()).collect();
    g.sort();
    assert_eq!(g, vec!["1", "2", "3", ru("grid_a"), ru("grid_b"), ru("grid_v")]);
    assert!(p.grids.iter().all(|g| g.named));
    let g2 = p.grids.iter().find(|g| g.name == "2").unwrap();
    assert!(near(g2.a, (6.0, -1.6), 1e-9) && near(g2.b, (6.0, 9.6), 1e-9));
    assert_eq!(p.columns.len(), 3, "{:?}", p.columns);
    assert_eq!(p.columns.iter().filter(|c| c.diameter.is_none()).count(), 2);
    for c in &p.columns {
        assert!(
            (c.b - 0.4).abs() < 1e-9 && (c.h - 0.4).abs() < 1e-9,
            "{c:?}"
        );
    }
    assert!(p
        .columns
        .iter()
        .any(|c| near(c.at, (9.0, 6.0), 1e-3) && c.diameter == Some(0.4)));
}

const TABLES: [&str; 9] = [
    "wall",
    "opening",
    "room",
    "grid",
    "col",
    "member_type",
    "prop",
    "room_sep",
    "op",
];

#[test]
fn fixture_accept_is_one_undo() {
    let (mut m, lv) = model();
    let (u, _) = Underlay::from_dxf_auto(&fixture::sample_plan(0));
    let tys = wall_types(&m).unwrap();
    let p = propose(&u, &tys, &ProposeOptions::default(), &Memory::default());
    let before = TABLES.map(|t| count(&m, t));
    let mut mem = Memory::default();
    let opt = AcceptOptions::new(lv);
    let rep = accept_proposals(&mut m, &p, &p.all_ids(), &opt, &mut mem).unwrap();
    assert_eq!(
        (rep.walls, rep.openings, rep.rooms, rep.grids, rep.columns),
        (5, 4, 2, 6, 3)
    );
    assert_eq!(rep.room_seps, 0, "outline lies along walls");
    assert_eq!(count(&m, "op") - before[8], 1);
    assert_eq!(count(&m, "grid"), before[3] + 6);
    assert_eq!(count(&m, "col"), before[4] + 3);
    // two sections: 400×400 and Ø400
    assert_eq!(count(&m, "member_type"), before[5] + 2);
    let fam: i64 = m
        .sql()
        .query_row(
            "SELECT count(*) FROM prop WHERE kind='opening' AND key='family'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fam, 4);
    m.undo().unwrap();
    assert_eq!(TABLES.map(|t| count(&m, t)), before);
}

#[test]
fn fixture_hatches_survive_model_round_trip() {
    let mut m = Model::open(":memory:").unwrap();
    let rep = dxf::import_into(&mut m, &fixture::sample_plan(0)).unwrap();
    assert_eq!(rep.hatches, 3);
    let out = dxf::export_from(&m).unwrap();
    let hatches: Vec<_> = out
        .entities
        .iter()
        .filter_map(|e| match e {
            Entity::Unknown(r) if r[0].value == "HATCH" => dxf::lower::hatch(r),
            _ => None,
        })
        .collect();
    assert_eq!(hatches.len(), 3);
    assert_eq!(hatches.iter().filter(|h| h.solid).count(), 2);
    assert!(hatches
        .iter()
        .all(|h| h.loops.len() == 1 && h.loops[0].len() == 4));
    let back = dxf::read_str(&dxf::write_string(&out)).unwrap();
    let n = back
        .entities
        .iter()
        .filter(|e| matches!(e, Entity::Unknown(r) if r[0].value == "HATCH"))
        .count();
    assert_eq!(n, 3);
}

#[test]
fn second_variant_moves_partition() {
    let (u, _) = Underlay::from_dxf_auto(&fixture::sample_plan(1));
    let p = propose(&u, &types(), &ProposeOptions::default(), &Memory::default());
    assert!(p
        .walls
        .iter()
        .any(|w| (w.a.0 - 4.0).abs() < 1e-6 && (w.b.0 - 4.0).abs() < 1e-6));
}

// ---- units, contours, names, recognition, accept ----

/// Lines `prims` in drawing units: coordinate × `k` + `off`.
fn doc_of(prims: &[Prim], k: f64, off: [f64; 2]) -> DxfDocument {
    let mut d = DxfDocument::default();
    for p in prims {
        if let Prim::Line(ly, a, b) = p {
            d.entities.push(Entity::Line(Line {
                common: Common::on_layer(ly),
                start: [a[0] * k + off[0], a[1] * k + off[1], 0.0],
                end: [b[0] * k + off[0], b[1] * k + off[1], 0.0],
            }));
        }
    }
    d
}

#[test]
fn guess_unit_per_axis_and_typical_sizes() {
    // metres in a local coordinate system: X ≈ 50, Y ≈ 3000 (X and Y used to be mixed → mm)
    assert_eq!(guess_unit(&doc_of(&room(), 1.0, [50.0, 3000.0])), 1.0);
    // metres with an outlier beyond 6000 km: wall thicknesses decide
    let mut d = doc_of(&room(), 1.0, [0.0, 0.0]);
    d.entities.push(Entity::Line(Line {
        common: Common::on_layer("0"),
        start: [6.0e6, 0.0, 0.0],
        end: [6.0e6 + 1.0, 0.0, 0.0],
    }));
    assert_eq!(guess_unit(&d), 1.0);
    // centimetres from a wall thickness of 20 units
    assert_eq!(guess_unit(&doc_of(&room(), 100.0, [0.0, 0.0])), 0.01);
    // the whole plan in mm inside one block (the extent used to be one insertion point → metres)
    let mut b = dxf::Block::new("PLAN");
    b.entities = doc_of(&room(), 1000.0, [0.0, 0.0]).entities;
    let mut d = DxfDocument::default();
    d.blocks.push(b);
    d.entities.push(Entity::Insert(dxf::Insert {
        common: Common::on_layer("0"),
        block: "PLAN".into(),
        ..Default::default()
    }));
    assert_eq!(guess_unit(&d), 0.001);
    // one door of 900 units: only mm give a 0.6–1.2 m door
    let mut b = dxf::Block::new("DOOR_900");
    b.entities = vec![
        Entity::Line(Line {
            common: Common::on_layer("0"),
            start: [0.0, 0.0, 0.0],
            end: [900.0, 0.0, 0.0],
        }),
        Entity::Line(Line {
            common: Common::on_layer("0"),
            start: [0.0, 0.0, 0.0],
            end: [0.0, 900.0, 0.0],
        }),
    ];
    let mut d = DxfDocument::default();
    d.blocks.push(b);
    d.entities.push(Entity::Insert(dxf::Insert {
        common: Common::on_layer("A-DOOR"),
        block: "DOOR_900".into(),
        ..Default::default()
    }));
    assert_eq!(guess_unit(&d), 0.001);
    // $MEASUREMENT = 0: inches
    let mut d = doc_of(&room(), 1.0 / 0.0254, [0.0, 0.0]);
    d.header.push(dxf::HeaderVar {
        name: "$MEASUREMENT".into(),
        values: vec![dxf::Pair::new(70, "0")],
    });
    assert_eq!(guess_unit(&d), 0.0254);
    // $INSUNITS still takes precedence
    d.header.push(dxf::HeaderVar {
        name: "$INSUNITS".into(),
        values: vec![dxf::Pair::new(70, "4")],
    });
    assert_eq!(guess_unit(&d), 0.001);
}

fn lw(layer: &str, pts: &[(f64, f64, f64)], flags: i32) -> Entity {
    Entity::LwPolyline(dxf::LwPolyline {
        common: Common::on_layer(layer),
        flags,
        vertices: pts
            .iter()
            .map(|&(x, y, bulge)| dxf::LwVertex {
                x,
                y,
                bulge,
                ..Default::default()
            })
            .collect(),
    })
}

#[test]
fn polylines_with_repeated_vertices_and_coincident_ends() {
    let mut d = DxfDocument::default();
    // a repeated vertex with bulge: the arc centre used to be NaN and leak into the outline
    d.entities.push(lw(
        "A-AREA",
        &[
            (0.0, 0.0, 0.0),
            (4.0, 0.0, 0.5),
            (4.0, 0.0, 0.0),
            (4.0, 4.0, 0.0),
            (0.0, 4.0, 0.0),
        ],
        1,
    ));
    // open, but the first point = the last: also an outline
    d.entities.push(lw(
        "A-AREA",
        &[
            (10.0, 0.0, 0.0),
            (14.0, 0.0, 0.0),
            (14.0, 4.0, 0.0),
            (10.0, 4.0, 0.0),
            (10.0, 0.0, 0.0),
        ],
        0,
    ));
    // open with different ends — not an outline
    d.entities.push(lw(
        "A-AREA",
        &[(20.0, 0.0, 0.0), (24.0, 0.0, 0.0), (24.0, 4.0, 0.0)],
        0,
    ));
    let u = Underlay::from_dxf(&d, 1.0);
    assert_eq!(u.closed.len(), 2, "{:?}", u.closed);
    for c in &u.closed {
        assert_eq!(c.pts.len(), 4, "{:?}", c.pts);
        assert!(c.pts.iter().all(|p| p.0.is_finite() && p.1.is_finite()));
    }
    assert!(u.arcs.iter().all(|a| a.r.is_finite() && a.c.0.is_finite()));
    // an outline with coinciding ends yields a room
    let mut rooms = vec![];
    rooms_from_outlines(&u, &mut rooms, &Memory::default());
    assert_eq!(rooms.len(), 2);
}

#[test]
fn unicode_escapes_in_text_layer_and_block_names() {
    let walls = "\\U+0421\\U+0422\\U+0415\\U+041D\\U+042B";
    let door = "\\U+0414\\U+0412\\U+0415\\U+0420\\U+042C_900";
    let mut d = DxfDocument::default();
    d.layers.push(Layer {
        name: walls.into(),
        flags: 0,
        color: 7,
        linetype: "CONTINUOUS".into(),
        extra: vec![],
    });
    d.entities.push(Entity::Line(Line {
        common: Common::on_layer(walls),
        start: [0.0, 0.0, 0.0],
        end: [1.0, 0.0, 0.0],
    }));
    d.entities.push(Entity::Text(Text {
        common: Common::on_layer("0"),
        text: "\\U+041A\\U+0443\\U+0445\\U+043D\\U+044F".into(),
        height: 0.3,
        ..Default::default()
    }));
    let mut b = dxf::Block::new(door);
    b.entities.push(Entity::Line(Line {
        common: Common::on_layer("0"),
        start: [0.0, 0.0, 0.0],
        end: [0.9, 0.0, 0.0],
    }));
    d.blocks.push(b);
    d.entities.push(Entity::Insert(dxf::Insert {
        common: Common::on_layer(walls),
        block: door.into(),
        pos: [5.0, 0.0, 0.0],
        ..Default::default()
    }));
    let u = Underlay::from_dxf(&d, 1.0);
    assert_eq!(u.lines[0].layer, ru("walls_layer_upper"));
    assert!(u.layers.iter().any(|l| l.name == ru("walls_layer_upper")));
    assert!(u.layers.iter().all(|l| !l.name.contains("\\U+")));
    assert_eq!(u.texts[0].text, ru("kitchen"));
    assert_eq!(u.inserts[0].name, ru("door_block_900"));
    assert_eq!(u.block_hints[0].kind, crate::dxf::BlockKind::Door);
    assert!(BlockRules::default().find(&u.inserts[0].name).is_some());
}

#[test]
fn layout_name_follows_acdblayout_subclass() {
    use crate::dxf::{Pair, RawSection};
    let mut d = DxfDocument::default();
    d.other_sections.push(RawSection {
        name: "OBJECTS".into(),
        pairs: vec![
            Pair::new(0, "LAYOUT"),
            Pair::new(100, "AcDbPlotSettings"),
            Pair::new(1, ""),
            Pair::new(100, "AcDbLayout"),
            Pair::new(1, ru("layout_2")),
            Pair::new(0, "LAYOUT"),
            Pair::new(100, "AcDbPlotSettings"),
            Pair::new(1, "A3 setup"),
            Pair::new(100, "AcDbLayout"),
            Pair::new(1, "Model"),
        ],
    });
    assert_eq!(d.layouts(), vec!["Model".to_string(), ru("layout_2").to_string()]);
}

fn rect_pts(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

fn room_at(id: u64, p: (f64, f64)) -> RoomCand {
    RoomCand {
        id,
        name: "R".into(),
        number: String::new(),
        point: p,
        area: 10.0,
        confidence: 0.9,
        boundary: vec![],
    }
}

#[test]
fn outline_goes_to_smallest_and_aggregate_outline_is_skipped() {
    let mut u = Underlay::default();
    for pts in [
        rect_pts(0.0, 0.0, 10.0, 10.0),
        rect_pts(0.0, 0.0, 5.0, 5.0),
        rect_pts(0.0, 0.0, 4.0, 4.0),
    ] {
        u.closed.push(UPoly {
            layer: "A-AREA".into(),
            pts,
            circle: None,
        });
    }
    let mut rooms = vec![room_at(1, (2.0, 2.0)), room_at(2, (7.0, 7.0))];
    rooms_from_outlines(&u, &mut rooms, &Memory::default());
    assert_eq!(rooms.len(), 2, "outline of the whole flat is not a room");
    assert_eq!(rooms[0].boundary, rect_pts(0.0, 0.0, 4.0, 4.0));
    assert!(rooms[1].boundary.is_empty());
}

fn wall_cand(id: u64, a: (f64, f64), b: (f64, f64), t: f64) -> WallCand {
    WallCand {
        id,
        a,
        b,
        thickness: t,
        height: 3.0,
        confidence: 0.9,
        wall_type: None,
        type_delta: None,
    }
}

fn solid(layer: &str, pts: Vec<(f64, f64)>) -> UFill {
    UFill {
        layer: layer.into(),
        loops: vec![pts],
        solid: true,
        pattern: "SOLID".into(),
        segs: vec![],
        pen: 0,
    }
}

#[test]
fn solid_fill_between_walls_is_not_a_column() {
    let grid = |id: u64, a: (f64, f64), b: (f64, f64)| GridCand {
        id,
        name: id.to_string(),
        a,
        b,
        named: false,
        confidence: 0.9,
    };
    let grids = vec![
        grid(1, (3.0, -5.0), (3.0, 5.0)),
        grid(2, (-5.0, 0.0), (10.0, 0.0)),
    ];
    let walls = vec![wall_cand(1, (0.0, 0.0), (6.0, 0.0), 0.3)];
    let mem = Memory::default();
    // a filled pier within a wall strip, even at an axis intersection
    let mut u = Underlay::default();
    u.fills
        .push(solid("A-HATCH", rect_pts(2.7, -0.15, 3.3, 0.15)));
    assert!(propose_columns(&u, &grids, &walls, &mem).is_empty());
    // the same fill without a wall — a column at an axis
    assert_eq!(propose_columns(&u, &grids, &[], &mem).len(), 1);
    // a standalone fill with no column layer and no axes — not a column
    let mut u = Underlay::default();
    u.fills
        .push(solid("A-HATCH", rect_pts(20.0, 20.0, 20.4, 20.4)));
    assert!(propose_columns(&u, &[], &walls, &mem).is_empty());
    // on a column layer — a column (the fill layer is kept)
    let mut u = Underlay::default();
    u.fills
        .push(solid("S-COLUMN", rect_pts(20.0, 20.0, 20.4, 20.4)));
    let c = propose_columns(&u, &[], &walls, &mem);
    assert_eq!(c.len(), 1);
    assert!((c[0].b - 0.4).abs() < 1e-9);
}

#[test]
fn column_layer_words_match_whole_tokens() {
    use super::extra::has_word;
    let cols: &[&str] = &["colu", ru("col_stem"), "col", "cols", ru("pylon_stem")];
    assert!(has_word("S-COL", cols));
    assert!(has_word("S-COLS", cols));
    assert!(has_word("A-Column-1", cols));
    assert!(has_word(ru("columns_word"), cols));
    assert!(has_word("COL1", cols));
    assert!(!has_word("COLOR", cols));
    assert!(!has_word("protocol", cols));
    // a wall layer that also holds columns is not skipped entirely
    let mut u = underlay_dxf(&room());
    for l in u.lines.iter_mut().filter(|l| l.layer == "A-WALL") {
        l.layer = ru("walls_and_columns").into();
    }
    let p = propose(&u, &types(), &ProposeOptions::default(), &Memory::default());
    assert!(!p.walls.is_empty());
}

#[test]
fn grid_id_does_not_depend_on_auto_name() {
    let gl = |x: f64| ULine {
        layer: "S-GRID".into(),
        a: (x, 0.0),
        b: (x, 10.0),
        dashed: false,
        pen: 0,
    };
    let mut u = Underlay::default();
    u.lines = vec![gl(0.0), gl(6.0)];
    let g1 = propose_grids(&u, &Memory::default());
    u.lines.push(gl(-6.0));
    let g2 = propose_grids(&u, &Memory::default());
    let id_at = |g: &[GridCand]| g.iter().find(|g| g.a.0.abs() < 1e-9).unwrap().id;
    assert_eq!(id_at(&g1), id_at(&g2));
    let n1 = g1.iter().find(|g| g.a.0.abs() < 1e-9).unwrap().name.clone();
    let n2 = g2.iter().find(|g| g.a.0.abs() < 1e-9).unwrap().name.clone();
    assert_ne!(n1, n2, "auto names shift, ids stay");
}

#[test]
fn block_opening_wider_than_wall_and_width_attribute() {
    let mut u = Underlay::default();
    u.inserts.push(UInsert {
        name: "DOOR_X".into(),
        layer: "A-DOOR".into(),
        pos: (0.0, 0.0),
        rotation: 0.0,
        size: (0.0, 0.0),
        center: (0.6, 0.0),
        attribs: vec![("DOOR_WIDTH".into(), "1200".into())],
    });
    let rules = BlockRules::default();
    let mem = Memory::default();
    let mut ops = vec![];
    let short = [wall_cand(1, (0.0, 0.0), (1.0, 0.0), 0.2)];
    assert_eq!(
        openings_from_blocks(&u, &rules, &short, &mut ops, (2.1, 0.9, 1.2), &mem),
        0
    );
    let long = [wall_cand(2, (0.0, 0.0), (3.0, 0.0), 0.2)];
    assert_eq!(
        openings_from_blocks(&u, &rules, &long, &mut ops, (2.1, 0.9, 1.2), &mem),
        1
    );
    assert!((ops[0].width - 1.2).abs() < 1e-9);
}

fn win_block(center: (f64, f64), attribs: &[(&str, &str)]) -> UInsert {
    UInsert {
        name: "WINDOW_A".into(),
        layer: "A-GLAZ".into(),
        pos: center,
        rotation: 0.0,
        size: (0.8, 0.2),
        center,
        attribs: attribs
            .iter()
            .map(|(t, v)| (t.to_string(), v.to_string()))
            .collect(),
    }
}

fn block_run(attribs: &[(&str, &str)], ops: &mut Vec<OpeningCand>) -> BlockOpenings {
    let mut u = Underlay::default();
    u.inserts.push(win_block((2.0, 0.0), attribs));
    let walls = [wall_cand(7, (0.0, 0.0), (4.0, 0.0), 0.2)];
    let rules = BlockRules::default();
    openings_from_blocks_checked(&u, &rules, &walls, ops, (2.1, 0.9, 1.2), &Memory::default())
}

#[test]
fn ambiguous_block_width_is_reported_and_flagged_not_silently_sized() {
    let mut ops = vec![];
    let r = block_run(&[("WIDTH", "1,200")], &mut ops);
    assert_eq!((r.added, r.unreadable_widths), (1, 1));
    assert_eq!(r.review, vec![ops[0].id]);
    assert!(ops[0].confidence < 0.5, "not auto-accepted: {:?}", ops[0]);
    // the value is never read as 1.2 or 1200; the geometric size is a placeholder for review
    assert!((ops[0].width - 0.8).abs() < 1e-9);
    // a non-number note is not a width at all: silent fallback as before
    let mut ops = vec![];
    let r = block_run(&[("WIDTH", "standard")], &mut ops);
    assert_eq!((r.unreadable_widths, r.review.len()), (0, 0));
    assert!((ops[0].confidence - 0.9).abs() < 1e-9);
}

#[test]
fn clear_and_missing_block_widths_behave_as_before() {
    let mut ops = vec![];
    let r = block_run(&[("W", "1200")], &mut ops);
    assert_eq!((r.added, r.unreadable_widths, r.review.len()), (1, 0, 0));
    assert!((ops[0].width - 1.2).abs() < 1e-9 && (ops[0].confidence - 0.9).abs() < 1e-9);
    let mut ops = vec![];
    let r = block_run(&[("W", "0,9")], &mut ops);
    assert_eq!(r.unreadable_widths, 0);
    assert!((ops[0].width - 0.9).abs() < 1e-9);
    // no width attribute: block size, not counted, not flagged
    let mut ops = vec![];
    let r = block_run(&[], &mut ops);
    assert_eq!((r.added, r.unreadable_widths, r.review.len()), (1, 0, 0));
    assert!((ops[0].width - 0.8).abs() < 1e-9 && (ops[0].confidence - 0.9).abs() < 1e-9);
}

#[test]
fn ambiguous_block_width_on_found_gap_keeps_gap_width_and_flags_it() {
    let gap = OpeningCand {
        id: 42,
        wall: 7,
        kind: "window".into(),
        along: 1.5,
        width: 1.0,
        sill: 0.9,
        height: 1.2,
        swing_side: false,
        swing_hinge: false,
        confidence: 0.8,
        family: String::new(),
    };
    let mut ops = vec![gap.clone()];
    let r = block_run(&[("WIDTH", "1,000")], &mut ops);
    assert_eq!((r.added, r.unreadable_widths, r.review.clone()), (0, 1, vec![42]));
    assert!((ops[0].width - 1.0).abs() < 1e-9 && ops[0].confidence < 0.5);
    // a clear attribute still refines the gap as before
    let mut ops = vec![gap];
    let r = block_run(&[("WIDTH", "1100")], &mut ops);
    assert_eq!(r.review.len(), 0);
    assert!((ops[0].width - 1.1).abs() < 1e-9 && ops[0].confidence >= 0.97);
}

#[test]
fn ambiguous_block_width_reaches_the_report() {
    let base = underlay_dxf(&room());
    let p0 = propose(&base, &types(), &ProposeOptions::default(), &Memory::default());
    let top = p0
        .walls
        .iter()
        .max_by(|a, b| (a.a.1 + a.b.1).total_cmp(&(b.a.1 + b.b.1)))
        .unwrap()
        .clone();
    let mid = ((top.a.0 + top.b.0) / 2.0, (top.a.1 + top.b.1) / 2.0);
    let run = |w: &str| {
        let mut u = base.clone();
        u.inserts.push(win_block(mid, &[("WIDTH", w)]));
        propose(&u, &types(), &ProposeOptions::default(), &Memory::default())
    };
    let p = run("1,200");
    assert_eq!(p.report.unreadable_numbers, 1, "{:?}", p.report);
    let o = p.openings.iter().find(|o| o.wall == top.id).expect("proposed");
    assert!(o.confidence < 0.5);
    let flags: Vec<_> = p.report.ambiguous.iter().filter(|a| a.id == o.id).collect();
    assert_eq!(flags.len(), 1, "{:?}", p.report.ambiguous);
    assert!(flags[0].reason.contains("width"));
    // same input, same result: the report does not depend on anything but the drawing
    assert_eq!(run("1,200"), p);
    let clear = run("1200");
    assert_eq!(clear.report.unreadable_numbers, 0);
    let o = clear.openings.iter().find(|o| o.wall == top.id).unwrap();
    assert!((o.width - 1.2).abs() < 1e-9 && o.confidence >= 0.9);
    assert!(clear.report.ambiguous.iter().all(|a| a.id != o.id));
}

#[test]
fn bad_proposals_are_skipped_not_fatal() {
    let (mut m, lv) = model();
    let mut p = Proposals::default();
    p.walls.push(wall_cand(1, (0.0, 0.0), (4.0, 0.0), 0.2));
    p.walls.push(wall_cand(2, (f64::NAN, 0.0), (4.0, 4.0), 0.2));
    let mut r = room_at(3, (2.0, 2.0));
    r.boundary = vec![(0.0, 0.0), (f64::NAN, 0.0), (4.0, 4.0)];
    p.rooms.push(r);
    p.columns.push(ColumnCand {
        id: 4,
        at: (8.0, 8.0),
        rotation: 0.0,
        diameter: None,
        b: 0.4,
        h: 0.4,
        confidence: 0.9,
    });
    let mut opt = AcceptOptions::new(lv);
    // column top = bottom: columns are skipped, the rest is accepted
    opt.top_level = Some(lv);
    let mut mem = Memory::default();
    let rep = accept_proposals(&mut m, &p, &p.all_ids(), &opt, &mut mem).unwrap();
    assert_eq!(
        (rep.walls, rep.rooms, rep.columns, rep.room_seps),
        (1, 1, 0, 0)
    );
    assert_eq!(
        (count(&m, "wall"), count(&m, "room"), count(&m, "col")),
        (1, 1, 0)
    );
    assert_eq!(rep.warnings.len(), 3, "{:?}", rep.warnings);
    assert!(!mem.rejected.contains(&2) && !mem.rejected.contains(&4));
}

#[test]
fn room_separators_skip_existing_model_walls() {
    let (mut m, lv) = model();
    let wtype: i64 = m
        .sql()
        .query_row("SELECT id FROM wall_type ORDER BY id LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    // a model wall along the bottom side of the outline (before acceptance)
    m.sql()
        .execute(
            "INSERT INTO wall(level,wtype,ax,ay,bx,by,base_offset,height,offset0) \
             VALUES (?1,?2,0,0,4,0,0,3,NULL)",
            rusqlite::params![lv, wtype],
        )
        .unwrap();
    let mut p = Proposals::default();
    let mut r = room_at(3, (2.0, 2.0));
    r.boundary = rect_pts(0.0, 0.0, 4.0, 4.0);
    p.rooms.push(r);
    let rep = accept_proposals(
        &mut m,
        &p,
        &[3],
        &AcceptOptions::new(lv),
        &mut Memory::default(),
    )
    .unwrap();
    assert_eq!(rep.room_seps, 3);
    // accepting the same outline again: the separators already exist
    let mut r = room_at(5, (2.0, 2.5));
    r.boundary = rect_pts(0.0, 0.0, 4.0, 4.0);
    p.rooms = vec![r];
    let rep = accept_proposals(
        &mut m,
        &p,
        &[5],
        &AcceptOptions::new(lv),
        &mut Memory::default(),
    )
    .unwrap();
    assert_eq!(rep.room_seps, 0);
}

#[test]
fn stale_family_prop_is_restored_by_undo() {
    let (mut m, lv) = model();
    // a property from a deleted opening with the same rowid
    m.sql()
        .execute(
            &format!(
                "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('opening',1,'family','{}','')",
                ru("old_family")
            ),
            [],
        )
        .unwrap();
    let mut p = Proposals::default();
    p.walls.push(wall_cand(1, (0.0, 0.0), (4.0, 0.0), 0.2));
    p.openings.push(OpeningCand {
        id: 2,
        wall: 1,
        kind: "door".into(),
        along: 1.0,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
        swing_side: false,
        swing_hinge: false,
        confidence: 0.9,
        family: ru("new_family").into(),
    });
    accept_proposals(
        &mut m,
        &p,
        &[2],
        &AcceptOptions::new(lv),
        &mut Memory::default(),
    )
    .unwrap();
    let fam = |m: &Model| -> Vec<String> {
        let mut st = m
            .sql()
            .prepare("SELECT value FROM prop WHERE kind='opening' AND ref=1 AND key='family'")
            .unwrap();
        let v: Vec<String> = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v
    };
    let oid: i64 = m
        .sql()
        .query_row("SELECT id FROM opening", [], |r| r.get(0))
        .unwrap();
    if oid == 1 {
        assert_eq!(fam(&m), vec![ru("new_family").to_string()]);
    }
    m.undo().unwrap();
    assert_eq!(fam(&m), vec![ru("old_family").to_string()]);
}

fn style_named(name: &str, height: f64) -> dxf::TextStyle {
    dxf::TextStyle {
        name: name.into(),
        flags: 0,
        height,
        width_factor: 1.0,
        oblique: 0.0,
        font: "txt".into(),
        big_font: String::new(),
        extra: vec![],
    }
}

fn zero_height_text_doc(style: Option<&str>, styles: Vec<dxf::TextStyle>, mtext: bool) -> DxfDocument {
    let mut d = DxfDocument::default();
    d.styles = styles;
    let common = Common::on_layer("0");
    d.entities.push(if mtext {
        Entity::MText(dxf::MText {
            common,
            pos: [1.0, 1.0, 0.0],
            height: 0.0,
            text: "label".into(),
            style: style.map(String::from),
            ..Default::default()
        })
    } else {
        Entity::Text(Text {
            common,
            pos: [1.0, 1.0, 0.0],
            height: 0.0,
            text: "label".into(),
            style: style.map(String::from),
            ..Default::default()
        })
    });
    d
}

#[test]
fn text_zero_height_uses_style_height() {
    let d = zero_height_text_doc(Some("Plan"), vec![style_named("PLAN", 0.4)], false);
    let u = Underlay::from_dxf(&d, 1.0);
    assert!((u.texts[0].height - 0.4).abs() < 1e-9, "{}", u.texts[0].height);
    let u = Underlay::from_dxf(&d, 0.001);
    assert!((u.texts[0].height - 0.0004).abs() < 1e-12);
}

#[test]
fn text_zero_height_no_style_falls_back_to_default() {
    let d = zero_height_text_doc(Some("Missing"), vec![style_named("Plan", 0.0)], false);
    let u = Underlay::from_dxf(&d, 1.0);
    assert!((u.texts[0].height - 0.0025).abs() < 1e-12, "{}", u.texts[0].height);
}

#[test]
fn mtext_zero_height_uses_style_height() {
    let d = zero_height_text_doc(Some("Plan"), vec![style_named("Plan", 0.3)], true);
    let u = Underlay::from_dxf(&d, 1.0);
    assert!((u.texts[0].height - 0.3).abs() < 1e-9, "{}", u.texts[0].height);
}
