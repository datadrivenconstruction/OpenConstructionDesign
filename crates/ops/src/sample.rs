// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Deterministic samples covering every wire variant. Used by this crate's tests and by
//! lowering and dry-run tests downstream.

use serde_json::json;

use crate::geo::*;
use crate::op::*;
use crate::refs::{Cat, Ref, Targets};
use crate::units::*;
use crate::{Actor, Envelope, Group, OP_VERSION};

fn m(v: f64) -> Len {
    Len::m(v)
}

fn rect() -> Profile {
    Profile::Rect { w: Len::mm(300.0), h: Len::mm(500.0) }
}

fn frame() -> Frame {
    Frame { origin: P3::m(0.0, 0.0, 0.0), x: Dir3::X, z: Dir3::Z }
}

fn line_path() -> Path {
    Path::Polyline { pts: vec![P3::m(0.0, 0.0, 0.0), P3::m(6.0, 0.0, 0.0)], bulges: vec![], closed: false }
}

fn square() -> Vec<Place> {
    vec![
        Place::At { p: P3::m(0.0, 0.0, 0.0) },
        Place::At { p: P3::m(6.0, 0.0, 0.0) },
        Place::At { p: P3::m(6.0, 4.0, 0.0) },
        Place::At { p: P3::m(0.0, 4.0, 0.0) },
    ]
}

fn l1() -> Ref {
    Ref::name(Cat::Level, "L1")
}

pub fn all_refs() -> Vec<Ref> {
    vec![
        Ref::id(Cat::Wall, 55),
        Ref::slot(1),
        Ref::Set { name: "south facade".into() },
        Ref::name(Cat::Grid, "A"),
        Ref::id(Cat::SlabType, 4),
        Ref::part(Ref::id(Cat::Wall, 55), "face:left"),
        Ref::part(Ref::part(Ref::slot(10), "edge:3"), "end:a"),
    ]
}

pub fn all_targets() -> Vec<Targets> {
    vec![
        Targets::Refs { refs: vec![Ref::id(Cat::Door, 7), Ref::id(Cat::Door, 8)] },
        Targets::Set { name: "south facade".into() },
        Targets::Filter { cat: Some(Cat::Wall), query: "level = 'L1' and height > 3 m".into() },
        Targets::Filter { cat: None, query: "mark = 'X'".into() },
    ]
}

pub fn all_actors() -> Vec<Actor> {
    vec![
        Actor::User,
        Actor::Xlsx,
        Actor::Script { name: "renumber.ops.json".into() },
        Actor::Ai { session: "s-0001".into(), model: "any".into() },
        Actor::Agent { name: "mcp:planner".into() },
    ]
}

pub fn all_places() -> Vec<Place> {
    vec![
        Place::At { p: P3::m(1.0, 2.0, 0.0) },
        Place::Offset { from: Ref::part(Ref::id(Cat::Wall, 55), "face:left"), d: Len::mm(600.0), side: Side::Left },
        Place::OnHost { host: Ref::id(Cat::Wall, 55), along: m(1.2), from_end: End::A, side: Side::Right, z: m(0.0) },
        Place::Crossing { a: Ref::name(Cat::Grid, "A"), b: Ref::name(Cat::Grid, "1") },
        Place::Center { of: Ref::id(Cat::Room, 3) },
        Place::AlignTo { refs: vec![Ref::id(Cat::Column, 4), Ref::id(Cat::Column, 5)] },
        Place::Between { a: Ref::id(Cat::Column, 4), b: Ref::id(Cat::Column, 5), t: 0.5 },
        Place::Snap {
            near: P3::m(3.0, 3.0, 0.0),
            kinds: vec![SnapKind::Endpoint, SnapKind::Midpoint, SnapKind::Intersection, SnapKind::Nearest,
                SnapKind::Perpendicular, SnapKind::Center, SnapKind::Grid, SnapKind::Face],
            max: Len::mm(250.0),
        },
    ]
}

pub fn all_profiles() -> Vec<Profile> {
    let mm = Len::mm;
    vec![
        rect(),
        Profile::Circle { r: mm(150.0) },
        Profile::Ring { r_out: mm(150.0), r_in: mm(120.0) },
        Profile::I { h: mm(300.0), b: mm(150.0), tw: mm(7.0), tf: mm(10.5) },
        Profile::H { h: mm(300.0), b: mm(300.0), tw: mm(10.0), tf: mm(15.0) },
        Profile::C { h: mm(200.0), b: mm(80.0), tw: mm(6.0), tf: mm(9.0) },
        Profile::T { h: mm(100.0), b: mm(100.0), tw: mm(8.0), tf: mm(8.0) },
        Profile::L { h: mm(100.0), b: mm(75.0), t: mm(8.0) },
        Profile::Z { h: mm(150.0), b: mm(60.0), t: mm(2.0) },
        Profile::Tube { w: mm(100.0), h: mm(100.0), t: mm(4.0) },
        Profile::Polygon { pts: vec![P2::m(0.0, 0.0), P2::m(1.0, 0.0), P2::m(0.0, 1.0)] },
        Profile::Polyline { pts: vec![P2::m(0.0, 0.0), P2::m(1.0, 0.0), P2::m(1.0, 1.0)], bulges: vec![0.0, 0.25, 0.0], closed: true },
        Profile::Rings {
            outer: vec![P2::m(0.0, 0.0), P2::m(2.0, 0.0), P2::m(2.0, 2.0), P2::m(0.0, 2.0)],
            holes: vec![vec![P2::m(0.5, 0.5), P2::m(1.5, 0.5), P2::m(1.5, 1.5)]],
        },
        Profile::Stored { of: Ref::name(Cat::MemberType, "HEA 200") },
    ]
}

pub fn all_forms() -> Vec<Form> {
    vec![
        Form::Extrude { profile: rect(), frame: frame(), start: m(0.0), end: m(3.0) },
        Form::Revolve {
            profile: Profile::Circle { r: Len::mm(50.0) },
            frame: frame(),
            axis_a: P3::m(0.0, 0.0, 0.0),
            axis_b: P3::m(0.0, 0.0, 1.0),
            angle: Angle::deg(270.0),
        },
        Form::Sweep {
            profile: rect(),
            path: Path::Ref { of: Ref::part(Ref::id(Cat::Wall, 5), "axis") },
            twist: None,
            scale: None,
        },
        Form::Loft {
            sections: vec![
                Section { profile: rect(), frame: frame() },
                Section {
                    profile: Profile::Circle { r: Len::mm(200.0) },
                    frame: Frame { origin: P3::m(0.0, 0.0, 3.0), x: Dir3::X, z: Dir3::Z },
                },
            ],
            path: None,
        },
        Form::Loft { sections: vec![Section { profile: rect(), frame: frame() }], path: Some(line_path()) },
        Form::SweptBlend { a: rect(), b: Profile::Circle { r: Len::mm(100.0) }, path: line_path() },
    ]
}

pub fn all_patterns() -> Vec<Pattern> {
    vec![
        Pattern::Linear { count: 5, dir: Dir3::X, spacing: Spacing::Step { d: m(6.0) } },
        Pattern::Linear { count: 4, dir: Dir3::Y, spacing: Spacing::Fit { total: m(18.0) } },
        Pattern::Radial { center: P3::m(0.0, 0.0, 0.0), axis: Dir3::Z, count: 8, angle: Angle::deg(360.0) },
        Pattern::AlongCurve {
            path: line_path(),
            count: Some(7),
            spacing: None,
            align: CurveAlign::Tangent,
            start_offset: Len::mm(500.0),
        },
        Pattern::AlongCurve { path: line_path(), count: None, spacing: Some(m(1.5)), align: CurveAlign::Fixed, start_offset: m(0.0) },
        Pattern::OnSurface {
            face: Ref::part(Ref::id(Cat::Roof, 2), "face:top"),
            u: Div::Count { n: 4 },
            v: Div::Step { d: m(1.2) },
            align: SurfAlign::Normal,
        },
        Pattern::OnSurface { face: Ref::part(Ref::id(Cat::Wall, 5), "face:right"), u: Div::Count { n: 2 }, v: Div::Count { n: 3 }, align: SurfAlign::Up },
        Pattern::Grid2d { nx: 4, ny: 3, dx: m(6.0), dy: m(7.5), angle: Angle::rad(0.0) },
        Pattern::Distribute { a: Ref::name(Cat::Grid, "A"), b: Ref::name(Cat::Grid, "B"), count: 3, ends: false },
    ]
}

pub fn all_xforms() -> Vec<Xform> {
    vec![
        Xform::Move { by: V3::m(0.0, 1.5, 0.0) },
        Xform::Rotate { center: P3::m(0.0, 0.0, 0.0), axis: Dir3::Z, angle: Angle::deg(90.0) },
        Xform::Mirror { a: P3::m(0.0, 0.0, 0.0), b: P3::m(0.0, 1.0, 0.0) },
    ]
}

fn datums() -> Group {
    let ops = vec![
        Op::Level(LevelOp::Create { slot: 1, name: "L2".into(), elevation: m(3.0) }),
        Op::Grid(GridOp::Create { slot: 2, name: "A".into(), a: P2::m(0.0, -1.0), b: P2::m(0.0, 20.0), bulge: 0.0 }),
        Op::Level(LevelOp::Rename { target: Ref::slot(1), name: "Level 2".into() }),
        Op::Level(LevelOp::SetElevation { target: l1(), elevation: Len::mm(-150.0) }),
        Op::Grid(GridOp::Rename { target: Ref::slot(2), name: "A1".into() }),
        Op::Param(ParamOp::Define {
            name: "FireRating".into(),
            kind: ParamKind::Text,
            unit: None,
            cats: vec![Cat::Door, Cat::Wall],
            instance: true,
            group: Some("identity".into()),
        }),
        Op::Param(ParamOp::Rename { from: "FireRating".into(), to: "Fire rating".into() }),
        Op::Param(ParamOp::Delete { name: "Old code".into() }),
        Op::Type(TypeOp::Duplicate {
            slot: 3,
            from: Ref::name(Cat::WallType, "Wall 200"),
            name: "Wall 250".into(),
            fields: vec![Field::new("thickness", Value::num(250.0, Unit::Mm))],
        }),
        Op::Type(TypeOp::Rename { target: Ref::slot(3), name: "Wall 250 brick".into() }),
        Op::Type(TypeOp::Set { target: Ref::slot(3), fields: vec![Field::new("material", Value::text("brick"))] }),
        Op::Type(TypeOp::Delete { target: Ref::id(Cat::WallType, 40), replace_with: Some(Ref::slot(3)) }),
        Op::Family(FamilyOp::Define {
            slot: 4,
            name: "Table".into(),
            cat: Cat::Component,
            params: vec![
                FamilyParam {
                    name: "W".into(),
                    kind: ParamKind::Length,
                    unit: Some(Unit::Mm),
                    default: Value::num(1200.0, Unit::Mm),
                    formula: None,
                    instance: true,
                },
                FamilyParam {
                    name: "Seats".into(),
                    kind: ParamKind::Integer,
                    unit: None,
                    default: Value::plain(json!(4)),
                    formula: Some("rounddown(W / 600 mm) * 2".into()),
                    instance: false,
                },
            ],
            planes: vec![
                RefPlaneDef { name: "left".into(), axis: Axis::X, offset: m(0.0), formula: None },
                RefPlaneDef { name: "right".into(), axis: Axis::X, offset: Len::mm(1200.0), formula: Some("W".into()) },
            ],
            forms: vec![FamilyForm {
                name: "top".into(),
                form: Form::Extrude { profile: rect(), frame: frame(), start: Len::mm(720.0), end: Len::mm(750.0) },
                visible_if: Some("W > 0".into()),
                material: Some("oak".into()),
            }],
            constraints: vec![FamilyRule { kind: RuleKind::Distance, planes: vec!["left".into(), "right".into()], param: Some("W".into()) }],
            tests: vec![
                vec![Field::new("W", Value::num(900.0, Unit::Mm))],
                vec![Field::new("W", Value::num(2.4, Unit::M))],
            ],
        }),
        Op::Family(FamilyOp::SetFormula { target: Ref::slot(4), param: "Seats".into(), formula: None }),
    ];
    Group { id: 1, title: "Datums, types, families".into(), ops, depends_on: vec![], why: String::new() }
}

fn elements() -> Group {
    let lv = || Ref::slot(1);
    let ty = || Some(Ref::slot(3));
    let ops = vec![
        Op::Create {
            slot: 10,
            el: NewEl::Wall {
                level: lv(),
                a: Place::At { p: P3::m(0.0, 0.0, 0.0) },
                b: Place::At { p: P3::m(6.0, 0.0, 0.0) },
                height: m(3.0),
                top: None,
                ty: ty(),
                fields: vec![],
            },
        },
        Op::Create {
            slot: 11,
            el: NewEl::Wall {
                level: lv(),
                a: Place::Offset { from: Ref::part(Ref::slot(10), "face:left"), d: m(4.0), side: Side::Left },
                b: Place::Crossing { a: Ref::name(Cat::Grid, "B"), b: Ref::slot(2) },
                height: Len::mm(2800.0),
                top: Some(l1()),
                ty: None,
                fields: vec![Field::new("mark", Value::text("W-2"))],
            },
        },
        Op::Create {
            slot: 12,
            el: NewEl::Door {
                host: Ref::slot(10),
                at: Place::OnHost { host: Ref::slot(10), along: m(1.2), from_end: End::A, side: Side::Left, z: m(0.0) },
                ty: Some(Ref::name(Cat::Type, "Door 900x2100")),
                fields: vec![Field::new("fire_rating", Value::text("EI30"))],
            },
        },
        Op::Create {
            slot: 13,
            el: NewEl::Window {
                host: Ref::slot(10),
                at: Place::Between { a: Ref::part(Ref::slot(10), "end:a"), b: Ref::part(Ref::slot(10), "end:b"), t: 0.75 },
                sill: Len::mm(900.0),
                ty: None,
                fields: vec![],
            },
        },
        Op::Create {
            slot: 14,
            el: NewEl::Opening { host: Ref::id(Cat::Wall, 55), at: Place::Center { of: Ref::id(Cat::Wall, 55) }, w: m(1.0), h: m(2.0), sill: m(0.0) },
        },
        Op::Create { slot: 15, el: NewEl::Slab { level: lv(), boundary: square(), offset: m(0.0), ty: None, fields: vec![] } },
        Op::Create {
            slot: 16,
            el: NewEl::Roof { level: lv(), boundary: square(), slope: Angle::deg(30.0), ty: None, fields: vec![] },
        },
        Op::Create {
            slot: 17,
            el: NewEl::Ceiling {
                level: lv(),
                boundary: vec![
                    Place::Snap { near: P3::m(0.0, 0.0, 0.0), kinds: vec![SnapKind::Endpoint], max: Len::mm(100.0) },
                    Place::AlignTo { refs: vec![Ref::slot(10), Ref::slot(11)] },
                    Place::At { p: P3::m(6.0, 4.0, 0.0) },
                ],
                height: m(2.7),
                ty: None,
                fields: vec![],
            },
        },
        Op::Create {
            slot: 18,
            el: NewEl::Column {
                level: lv(),
                at: Place::Crossing { a: Ref::slot(2), b: Ref::name(Cat::Grid, "1") },
                top: Some(l1()),
                height: m(3.0),
                rotation: Angle::deg(0.0),
                ty: Some(Ref::name(Cat::MemberType, "C 400x400")),
                fields: vec![],
            },
        },
        Op::Create {
            slot: 19,
            el: NewEl::Beam {
                level: lv(),
                a: Place::Center { of: Ref::slot(18) },
                b: Place::At { p: P3::m(6.0, 0.0, 3.0) },
                ty: None,
                fields: vec![],
            },
        },
        Op::Create {
            slot: 20,
            el: NewEl::Stair {
                level: l1(),
                a: Place::At { p: P3::m(1.0, 1.0, 0.0) },
                b: Place::At { p: P3::m(1.0, 5.0, 0.0) },
                top: Some(lv()),
                width: Len::mm(1200.0),
                ty: None,
                fields: vec![Field::new("risers", Value::plain(json!(18)))],
            },
        },
        Op::Create {
            slot: 21,
            el: NewEl::Railing {
                level: l1(),
                path: vec![Place::At { p: P3::m(1.0, 1.0, 0.0) }, Place::At { p: P3::m(1.0, 5.0, 0.0) }],
                host: Some(Ref::slot(20)),
                height: Len::mm(1100.0),
                ty: None,
                fields: vec![],
            },
        },
        Op::Create {
            slot: 22,
            el: NewEl::Room { level: lv(), at: Place::At { p: P3::m(3.0, 2.0, 0.0) }, name: "Kitchen".into(), number: Some("3.04".into()) },
        },
        Op::Create {
            slot: 23,
            el: NewEl::Component {
                level: lv(),
                at: Place::Center { of: Ref::slot(22) },
                family: Ref::slot(4),
                host: None,
                rotation: Angle::deg(90.0),
                ty: None,
                fields: vec![Field::new("W", Value::num(1.6, Unit::M))],
            },
        },
        Op::Create {
            slot: 24,
            el: NewEl::MepRun {
                level: lv(),
                system: "supply air".into(),
                path: vec![Place::At { p: P3::m(0.0, 2.0, 2.9) }, Place::At { p: P3::m(6.0, 2.0, 2.9) }],
                size: Len::mm(400.0),
                ty: None,
                fields: vec![],
            },
        },
        Op::Create {
            slot: 25,
            el: NewEl::Generic {
                cat: Cat::Footing,
                level: Some(l1()),
                at: vec![Place::Center { of: Ref::slot(18) }],
                ty: None,
                fields: vec![Field::new("depth", Value::num(600.0, Unit::Mm))],
            },
        },
        Op::Set {
            target: Targets::Refs { refs: vec![Ref::slot(10), Ref::slot(11)] },
            fields: vec![
                Field::new("height", Value::num(3.2, Unit::M)),
                Field::new("comment", Value::text("by option A")),
                Field::new("load_bearing", Value::plain(json!(true))),
            ],
        },
        Op::SetType { target: Targets::Filter { cat: Some(Cat::Door), query: "level = 'L1'".into() }, to: Ref::name(Cat::Type, "Door 1000x2100") },
        Op::Transform { target: Targets::one(Ref::slot(19)), xf: Xform::Move { by: V3::m(0.0, 0.5, 0.0) }, copy: false },
        Op::Transform {
            target: Targets::Set { name: "furniture".into() },
            xf: Xform::Rotate { center: P3::m(3.0, 2.0, 0.0), axis: Dir3::Z, angle: Angle::deg(45.0) },
            copy: true,
        },
        Op::Transform { target: Targets::one(Ref::slot(23)), xf: Xform::Mirror { a: P3::m(3.0, 0.0, 0.0), b: P3::m(3.0, 1.0, 0.0) }, copy: false },
        Op::Pattern {
            source: Targets::one(Ref::slot(18)),
            pattern: Pattern::Linear { count: 4, dir: Dir3::X, spacing: Spacing::Step { d: m(6.0) } },
            out: PatternOut::Copies,
        },
        Op::Pattern {
            source: Targets::one(Ref::slot(23)),
            pattern: Pattern::Distribute { a: Ref::slot(10), b: Ref::slot(11), count: 3, ends: false },
            out: PatternOut::Element,
        },
        Op::Geometry(GeoOp::Solid {
            slot: 30,
            level: Some(lv()),
            form: Form::Extrude { profile: rect(), frame: frame(), start: m(0.0), end: m(3.0) },
            material: Some("concrete".into()),
            name: None,
            ifc: None,
            role: None,
        }),
        Op::Geometry(GeoOp::Mass {
            slot: 31,
            level: None,
            form: Form::Loft { sections: vec![Section { profile: rect(), frame: frame() }], path: Some(line_path()) },
            name: Some("Tower".into()),
        }),
        Op::Geometry(GeoOp::Boolean {
            kind: BoolKind::Cut,
            a: Ref::slot(30),
            b: Tool::Form { form: Form::Extrude { profile: Profile::Circle { r: Len::mm(100.0) }, frame: frame(), start: m(0.0), end: m(3.0) } },
            keep_tool: false,
        }),
        Op::Geometry(GeoOp::Boolean { kind: BoolKind::Union, a: Ref::slot(30), b: Tool::Ref { of: Ref::slot(31) }, keep_tool: true }),
        Op::Geometry(GeoOp::Boolean { kind: BoolKind::Intersect, a: Ref::slot(31), b: Tool::Ref { of: Ref::id(Cat::Mass, 9) }, keep_tool: true }),
        Op::Geometry(GeoOp::Void {
            host: Ref::slot(10),
            form: Form::Extrude { profile: rect(), frame: frame(), start: m(-0.5), end: m(0.5) },
        }),
        Op::Geometry(GeoOp::Reshape {
            target: Ref::slot(31),
            form: Form::SweptBlend { a: rect(), b: Profile::Stored { of: Ref::name(Cat::MemberType, "HEA 200") }, path: line_path() },
        }),
        Op::Geometry(GeoOp::Convert { target: Ref::id(Cat::Mesh, 77), to: ConvertTo::Parametric }),
        Op::Geometry(GeoOp::Freeform {
            slot: 32,
            level: Some(l1()),
            surface: SurfaceBy::Loft {
                curves: (0..3)
                    .map(|k| Curve3 {
                        pts: vec![
                            P3::m(0.0, 4.0 * k as f64, 6.0),
                            P3::m(6.0, 4.0 * k as f64, 7.5 + k as f64),
                            P3::m(12.0, 4.0 * k as f64, 6.0),
                        ],
                        fit: Some(CurveFit::Through),
                        degree: Some(2),
                    })
                    .collect(),
                degree: Some(2),
                params: None,
            },
            thickness: Some(Len::mm(150.0)),
            side: Some(ShellSide::Reverse),
            trim: None,
            material: Some("Concrete".into()),
            name: Some("Roof shell".into()),
        }),
        Op::Geometry(GeoOp::EditPoint { target: Ref::slot(32), i: 1, j: 1, to: P3::m(6.0, 4.0, 9.0) }),
        Op::Geometry(GeoOp::Thicken { target: Ref::slot(32), thickness: Len::mm(200.0), side: Some(ShellSide::Center) }),
        Op::Geometry(GeoOp::Trim {
            target: Ref::slot(32),
            trim: Some(UvTrim { outer: None, holes: vec![vec![[0.4, 0.4], [0.6, 0.4], [0.6, 0.6], [0.4, 0.6]]] }),
        }),
        Op::Geometry(GeoOp::Rebuild { target: Ref::slot(32), count_u: 6, count_v: 5, degree_u: Some(3), degree_v: None }),
        Op::Geometry(GeoOp::Freeform {
            slot: 33,
            level: Some(l1()),
            surface: SurfaceBy::Network {
                u_curves: (0..3)
                    .map(|k| Curve3 {
                        pts: vec![
                            P3::m(0.0, 5.0 * k as f64, 6.0),
                            P3::m(5.0, 5.0 * k as f64, 7.0),
                            P3::m(10.0, 5.0 * k as f64, 6.0),
                        ],
                        fit: Some(CurveFit::Through),
                        degree: Some(2),
                    })
                    .collect(),
                v_curves: (0..3)
                    .map(|k| Curve3 {
                        pts: vec![
                            P3::m(5.0 * k as f64, 0.0, [6.0, 7.0, 6.0][k]),
                            P3::m(5.0 * k as f64, 5.0, [6.0, 7.0, 6.0][k]),
                            P3::m(5.0 * k as f64, 10.0, [6.0, 7.0, 6.0][k]),
                        ],
                        fit: Some(CurveFit::Polyline),
                        degree: None,
                    })
                    .collect(),
                degree: Some(3),
                tolerance: Some(Len::mm(1.0)),
            },
            thickness: None,
            side: None,
            trim: None,
            material: None,
            name: Some("Canopy".into()),
        }),
        Op::Geometry(GeoOp::Panelize {
            slot: 34,
            host: Ref::slot(33),
            columns: PanelDiv::Width { width: Len::mm(1500.0) },
            rows: PanelDiv::Equal { n: 6 },
            pattern: Some(PanelPattern::Quad),
            planarize: Some(100),
            mullion: Some(Mullion { width: Len::mm(60.0), depth: Len::mm(150.0) }),
            glass: Some(Len::mm(24.0)),
            flat: Some(Len::mm(1.0)),
            cold: Some(Len::mm(20.0)),
            material: Some("Glass".into()),
            name: Some("Canopy glazing".into()),
        }),
        Op::Geometry(GeoOp::Freeform {
            slot: 35,
            level: Some(l1()),
            surface: SurfaceBy::Dome { center: P3::m(30.0, 10.0, 6.0), radius: Len::m(6.0), rise: Len::m(3.0) },
            thickness: None,
            side: None,
            trim: None,
            material: None,
            name: Some("Dome".into()),
        }),
        Op::Geometry(GeoOp::Freeform {
            slot: 36,
            level: Some(l1()),
            surface: SurfaceBy::Vault {
                a: P3::m(40.0, 0.0, 6.0),
                b: P3::m(52.0, 0.0, 6.0),
                span: Len::m(5.0),
                rise: Len::m(1.5),
            },
            thickness: None,
            side: None,
            trim: None,
            material: None,
            name: Some("Vault".into()),
        }),
        Op::Geometry(GeoOp::SurfaceShell {
            slot: 37,
            host: Ref::slot(35),
            kind: ShellKind::Roof,
            layers: vec![
                ShellLayer { thickness: Len::mm(150.0), material: Some("Concrete".into()), name: Some("Structure".into()) },
                ShellLayer { thickness: Len::mm(100.0), material: Some("Mineral wool".into()), name: None },
            ],
            side: Some(ShellSide::Normal),
            openings: Some(vec![vec![P2::m(31.0, 11.0), P2::m(32.0, 11.0), P2::m(32.0, 12.0), P2::m(31.0, 12.0)]]),
            name: Some("Dome roof".into()),
        }),
        Op::Geometry(GeoOp::Gridshell {
            slot: 38,
            level: Some(l1()),
            host: LatticeHost::Dome {
                center: P3::m(60.0, 10.0, 3.0),
                radius: Len::m(12.0),
                rise: Len::m(5.0),
                oculus: Some(Len::m(1.5)),
            },
            kind: Some(LatticeKind::Diagrid),
            pattern: Some(LatticePattern::Diagrid),
            count_u: 16,
            count_v: 4,
            supports: Some(LatticeSupports::Base),
            member: "CHS 88.9x5".into(),
            edge: Some("CHS 168.3x8".into()),
            node: None,
        }),
        Op::Geometry(GeoOp::Member {
            slot: 39,
            level: Some(l1()),
            start: P3::m(70.0, 0.0, 3.0),
            end: P3::m(76.0, 0.0, 3.0),
            section: "IPE 300".into(),
            role: MemberRole::Beam,
            rotation: None,
        }),
        Op::Geometry(GeoOp::Plate {
            slot: 42,
            level: Some(l1()),
            outline: vec![P3::m(70.0, 5.0, 0.0), P3::m(74.0, 5.0, 0.0), P3::m(74.0, 5.0, 3.0), P3::m(70.0, 5.0, 3.0)],
            thickness: Len::mm(8.0),
            openings: vec![vec![P3::m(71.0, 5.0, 1.0), P3::m(72.0, 5.0, 1.0), P3::m(72.0, 5.0, 2.0)]],
            material: Some("S355".into()),
        }),
        Op::Constrain {
            rule: Rule {
                kind: RuleKind::Distance,
                refs: vec![Ref::part(Ref::slot(10), "face:left"), Ref::part(Ref::slot(11), "face:right")],
                value: Some(Value::num(4.0, Unit::M)),
            },
        },
        Op::Constrain { rule: Rule { kind: RuleKind::Align, refs: vec![Ref::slot(18), Ref::slot(2)], value: None } },
        Op::Delete { target: Targets::one(Ref::id(Cat::Wall, 56)) },
    ];
    Group { id: 2, title: "Elements".into(), ops, depends_on: vec![1], why: String::new() }
}

fn options() -> Group {
    let ops = vec![
        Op::Option(OptionOp::CreateSet { slot: 40, name: "Kitchen layout".into() }),
        Op::Option(OptionOp::Add { slot: 41, set: Ref::slot(40), name: "Island".into() }),
        Op::Option(OptionOp::Assign { target: Targets::one(Ref::slot(23)), option: Ref::slot(41) }),
        Op::Option(OptionOp::Accept { option: Ref::slot(41) }),
    ];
    Group { id: 3, title: "Options".into(), ops, depends_on: vec![2], why: String::new() }
}

/// Envelope with every op kind and every `NewEl` family; passes `check()`.
pub fn envelope() -> Envelope {
    Envelope {
        v: OP_VERSION,
        actor: Actor::Ai { session: "s-0001".into(), model: "any".into() },
        title: "Sample: every op".into(),
        why: Some("wire coverage".into()),
        groups: vec![datums(), elements(), options()],
    }
}

pub fn all_ops() -> Vec<Op> {
    envelope().groups.into_iter().flat_map(|g| g.ops).collect()
}
