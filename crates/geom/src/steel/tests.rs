// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The ten connections on fixture members: each builds, meshes, keeps the catalogue rules and
//! has no clashing bolts; skewed members and a changed section give an adapted detail.

use super::catalog::catalog;
use super::check::{check, errors};
use super::forces::{verdict, Verdict};
use super::schedule::schedule;
use super::*;
use crate::gsolid::{mesh, VIEW_SAG};
use crate::math::v3;
use crate::member::{Just, Path, Shape};

pub(crate) fn ipe(h: f64, b: f64, tw: f64, tf: f64) -> Shape {
    Shape::I { h, b, tw, tf }
}
pub(crate) fn heb200() -> Shape {
    ipe(0.2, 0.2, 0.009, 0.015)
}
pub(crate) fn ipe300() -> Shape {
    ipe(0.3, 0.15, 0.0071, 0.0107)
}
pub(crate) fn ipe200() -> Shape {
    ipe(0.2, 0.1, 0.0056, 0.0085)
}
pub(crate) fn pipe(d: f64, t: f64) -> Shape {
    Shape::Pipe { d, t }
}

pub(crate) fn bar(a: V3, b: V3, shape: Shape, column: bool) -> Member {
    Member {
        shape,
        path: Path::Line { a, b },
        rotation: 0.0,
        yjust: Just::Center,
        zjust: Just::Center,
        column,
        start: None,
        end: None,
    }
}

pub(crate) fn col(x: f64, y: f64, z0: f64, z1: f64, shape: Shape) -> Member {
    bar(v3(x, y, z0), v3(x, y, z1), shape, true)
}

/// Builds, meshes every part and checks the rules; returns the connection.
pub(crate) fn sound(kind: ConnKind, ms: &[Member], p: &Params) -> Conn {
    let c = build(kind, ms, p).unwrap_or_else(|e| panic!("{}: {e}", kind.key()));
    assert!(!c.parts.is_empty(), "{}: no parts", kind.key());
    for (i, part) in c.parts.iter().enumerate() {
        let m = mesh(&part.form, &part.frame, VIEW_SAG)
            .unwrap_or_else(|e| panic!("{} part {i} {}: {e:?}", kind.key(), part.mark));
        assert!(
            !m.triangles.is_empty(),
            "{} part {i} {} is empty",
            kind.key(),
            part.mark
        );
        assert!(part.frame.is_valid(), "{} part {i}: bad frame", kind.key());
    }
    let f = check(&c, ms);
    let bad: Vec<_> = f
        .iter()
        .filter(|x| x.rule.is_error())
        .map(|x| &x.text)
        .collect();
    assert_eq!(errors(&f), 0, "{}: {bad:#?}", kind.key());
    c
}

fn count(c: &Conn, r: Role) -> usize {
    c.parts.iter().filter(|p| p.role == r).count()
}

fn column_beam(angle_deg: f64, beam: Shape) -> Vec<Member> {
    // beam from 4 m away, square or skewed in plan, onto the column flange (world y faces)
    let a = angle_deg.to_radians();
    let end = v3(0.0, 0.0, 3.0);
    let far = v3(4.0 * a.sin(), 4.0 * a.cos(), 3.0);
    vec![
        col(0.0, 0.0, 0.0, 4.0, heb200()),
        bar(far, end, beam, false),
    ]
}

#[test]
fn catalogue_reads_and_keeps_its_series() {
    let cat = catalog();
    let m20 = cat.bolt(0.020).expect("M20");
    assert!((m20.hole - 0.022).abs() < 1e-9);
    assert_eq!(cat.bolts.first().map(|b| b.name()), Some("M12".to_string()));
    assert_eq!(cat.bolts.last().map(|b| b.name()), Some("M36".to_string()));
    assert!(cat.class("8.8").is_some() && cat.class("10.9").is_some());
    assert!(cat.weld_leg(0.012, 0.010).is_some());
    for b in &cat.bolts {
        assert!(b.hole > b.d && b.anchor_hole > b.hole && b.w_id > b.d && b.w_od > b.s);
    }
}

#[test]
fn params_round_trip_and_refuse_unknown_keys() {
    let p = Params::parse("bolt=M20;class=10.9;rows=3;variant=end_plate;plate=15").unwrap();
    assert_eq!(p.bolt, Some(0.02));
    assert_eq!(Params::parse(&p.text()).unwrap(), p);
    assert!(Params::parse("bolts=3").is_err());
    assert!(Params::parse("class=7.7").is_err());
    assert!(ConnKind::ALL
        .iter()
        .all(|k| ConnKind::parse(k.key()) == Some(*k)));
}

#[test]
fn base_plate_on_columns_of_every_kind() {
    for (shape, anchors) in [
        (heb200(), 4),
        (ipe(0.5, 0.3, 0.0145, 0.028), 6),
        (pipe(0.1683, 0.008), 4),
        (
            Shape::Tube {
                h: 0.15,
                b: 0.15,
                t: 0.008,
            },
            4,
        ),
    ] {
        let c = sound(
            ConnKind::BasePlate,
            &[col(1.0, 2.0, 0.0, 3.0, shape)],
            &Params::default(),
        );
        assert_eq!(c.bolts.iter().filter(|b| b.1.anchor).count(), anchors);
        assert_eq!(count(&c, Role::Plate), 1);
        assert!(c.parts.iter().any(|p| p.role == Role::Weld));
    }
}

#[test]
fn base_plate_turns_with_a_rotated_column() {
    let mut m = col(0.0, 0.0, 0.0, 3.0, heb200());
    m.rotation = 30f64.to_radians();
    let c = sound(ConnKind::BasePlate, &[m], &Params::default());
    let plate = c.parts.iter().find(|p| p.role == Role::Plate).unwrap();
    let a = plate.frame.x.y.atan2(plate.frame.x.x).to_degrees();
    assert!((a.abs() - 30.0).abs() < 1e-6, "plate turned {a}");
}

#[test]
fn pinned_fin_and_end_plate_square_and_skewed() {
    for angle in [0.0, 20.0, -35.0] {
        let ms = column_beam(angle, ipe300());
        let c = sound(ConnKind::Pinned, &ms, &Params::default());
        assert!(c.bolts.len() >= 2, "angle {angle}");
        assert_eq!(
            angle != 0.0,
            c.notes.iter().any(|n| n.contains("skewed")),
            "angle {angle}"
        );
        let p = Params {
            variant: Some("end_plate".into()),
            ..Params::default()
        };
        let c = sound(ConnKind::Pinned, &ms, &p);
        assert_eq!(c.trims.len(), 1);
    }
}

#[test]
fn pinned_regenerates_for_a_new_section() {
    let small = sound(
        ConnKind::Pinned,
        &column_beam(0.0, ipe200()),
        &Params::default(),
    );
    let deep = ipe(0.5, 0.2, 0.0102, 0.016);
    let big = sound(
        ConnKind::Pinned,
        &column_beam(0.0, deep),
        &Params::default(),
    );
    assert!(big.bolts.len() > small.bolts.len());
    let depth = |c: &Conn| match &c.parts[0].qty {
        PartQty::Plate { outline, .. } => part::extent(outline).1,
        _ => 0.0,
    };
    assert!(depth(&big) > depth(&small) + 0.05);
}

#[test]
fn rigid_end_plate_with_stiffeners_and_on_a_sloping_rafter() {
    let c = sound(
        ConnKind::RigidEndPlate,
        &column_beam(0.0, ipe300()),
        &Params::default(),
    );
    assert_eq!(count(&c, Role::Stiffener), 4);
    assert!(c.bolts.len() >= 6);
    // rafter rising at 10 degrees
    let col_m = col(0.0, 0.0, 0.0, 4.0, heb200());
    let raf = bar(
        v3(0.0, 5.0, 3.0 + 5.0 * 10f64.to_radians().tan()),
        v3(0.0, 0.0, 3.0),
        ipe300(),
        false,
    );
    let c = sound(ConnKind::RigidEndPlate, &[col_m, raf], &Params::default());
    let pl = c.parts.iter().find(|p| p.role == Role::Plate).unwrap();
    assert!(
        pl.frame.z.z.abs() < 1e-9,
        "the plate stays on the vertical face"
    );
}

#[test]
fn beam_splice_by_covers_and_with_a_depth_change() {
    let a = bar(v3(0.0, 0.0, 3.0), v3(3.0, 0.0, 3.0), ipe300(), false);
    let b = bar(v3(3.0, 0.0, 3.0), v3(6.0, 0.0, 3.0), ipe300(), false);
    let c = sound(ConnKind::BeamSplice, &[a.clone(), b], &Params::default());
    assert_eq!(count(&c, Role::Plate), 4);
    let b2 = bar(
        v3(3.0, 0.0, 3.0),
        v3(6.0, 0.0, 3.0),
        ipe(0.27, 0.135, 0.0066, 0.0102),
        false,
    );
    let c = sound(ConnKind::BeamSplice, &[a, b2], &Params::default());
    assert_eq!(count(&c, Role::Packer), 2);
}

#[test]
fn column_splice_by_covers_and_by_end_plates() {
    let lo = col(0.0, 0.0, 0.0, 3.0, heb200());
    let hi = col(0.0, 0.0, 3.0, 6.0, heb200());
    sound(ConnKind::ColumnSplice, &[lo, hi], &Params::default());
    let lo = col(
        0.0,
        0.0,
        0.0,
        3.0,
        Shape::Tube {
            h: 0.15,
            b: 0.15,
            t: 0.008,
        },
    );
    let hi = col(
        0.0,
        0.0,
        3.0,
        6.0,
        Shape::Tube {
            h: 0.15,
            b: 0.15,
            t: 0.008,
        },
    );
    let c = sound(ConnKind::ColumnSplice, &[lo, hi], &Params::default());
    assert_eq!(c.trims.len(), 2);
    let lo = col(0.0, 0.0, 0.0, 3.0, pipe(0.1683, 0.008));
    let hi = col(0.0, 0.0, 3.0, 6.0, pipe(0.1683, 0.008));
    let c = sound(ConnKind::ColumnSplice, &[lo, hi], &Params::default());
    assert!(c.bolts.len() >= 4);
}

#[test]
fn truss_gusset_takes_skewed_web_members() {
    let chord = bar(
        v3(-3.0, 0.0, 0.0),
        v3(3.0, 0.0, 0.0),
        Shape::Tee {
            h: 0.1,
            b: 0.2,
            tw: 0.008,
            tf: 0.012,
        },
        false,
    );
    let mut chord = chord;
    chord.rotation = std::f64::consts::PI;
    let angle = Shape::Tube {
        h: 0.08,
        b: 0.08,
        t: 0.005,
    };
    let d1 = bar(v3(0.0, 0.0, 0.0), v3(-1.5, 0.0, 1.8), angle.clone(), false);
    let v = bar(v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.8), angle.clone(), false);
    let d2 = bar(v3(0.0, 0.0, 0.0), v3(2.4, 0.0, 1.8), angle, false);
    let c = sound(
        ConnKind::TrussGusset,
        &[chord, d1, v, d2],
        &Params::default(),
    );
    assert_eq!(count(&c, Role::Gusset), 1);
    assert_eq!(c.bolts.len(), 6);
    assert_eq!(c.trims.len(), 3);
    knife_plated(&c, 3);
}

#[test]
fn bracing_gusset_at_mid_beam_and_at_a_corner() {
    let beam = bar(v3(-3.0, 0.0, 4.0), v3(3.0, 0.0, 4.0), ipe300(), false);
    let br1 = bar(
        v3(-3.0, 0.0, 0.0),
        v3(0.0, 0.0, 4.0),
        pipe(0.0889, 0.004),
        false,
    );
    let br2 = bar(
        v3(3.0, 0.0, 0.0),
        v3(0.0, 0.0, 4.0),
        pipe(0.0889, 0.004),
        false,
    );
    let c = sound(
        ConnKind::BracingGusset,
        &[beam, br1, br2],
        &Params::default(),
    );
    assert_eq!(c.bolts.len(), 4);
    knife_plated(&c, 2);
    let column = col(0.0, 0.0, 0.0, 4.0, heb200());
    let top = bar(v3(0.0, 0.0, 4.0), v3(0.0, 5.0, 4.0), ipe300(), false);
    let brace = bar(
        v3(0.0, 0.0, 4.0),
        v3(0.0, 5.0, 0.0),
        pipe(0.0889, 0.004),
        false,
    );
    let c = sound(
        ConnKind::BracingGusset,
        &[column, top, brace],
        &Params::default(),
    );
    assert_eq!(count(&c, Role::Gusset), 1);
    // welded to both hosts, and the brace into its slot
    assert!(count(&c, Role::Weld) >= 4 + 4);
    knife_plated(&c, 1);
    // a 300 mm box brace on a column: the bolts still take only the two plates
    let column = col(0.0, 0.0, 0.0, 6.0, heb200());
    let boxes = Shape::Tube {
        h: 0.3,
        b: 0.3,
        t: 0.0125,
    };
    let brace = bar(v3(0.0, 0.0, 3.0), v3(0.0, 6.0, 0.0), boxes, false);
    let c = sound(
        ConnKind::BracingGusset,
        &[column, brace],
        &Params::default(),
    );
    knife_plated(&c, 1);
}

/// Hollow braces are never bolted through: every bolt takes the gusset and a knife plate only,
/// a stock length covers it, and each knife plate is welded into its slot along both walls.
fn knife_plated(c: &Conn, braces: usize) {
    let cat = catalog();
    let t = |i: usize| match &c.parts[i].qty {
        PartQty::Plate { t, .. } => *t,
        _ => panic!("bolt ply {i} is not a plate"),
    };
    assert!(!c.bolts.is_empty());
    for (_, b) in &c.bolts {
        let kinds: Vec<Role> = b.plies.iter().map(|i| c.parts[*i].role).collect();
        assert_eq!(kinds, [Role::Gusset, Role::Plate]);
        let plies: f64 = b.plies.iter().map(|i| t(*i)).sum();
        assert!((b.grip - plies).abs() < 1e-9, "grip {} over {plies}", b.grip);
        let need = cat.bolt_need(&b.size, b.grip);
        assert!(cat.bolt_length(need).is_some(), "grip {} has no stock bolt", b.grip);
    }
    assert_eq!(count(c, Role::Plate), braces);
    assert!(count(c, Role::Weld) >= 4 * braces, "two walls, both plate faces");
    let notes = c.notes.iter().filter(|n| n.contains("knife plate")).count();
    assert_eq!(notes, braces);
}

#[test]
fn purlin_cleat_on_a_rafter() {
    let raf = bar(v3(0.0, -4.0, 3.0), v3(0.0, 4.0, 3.0), ipe300(), false);
    let ch = Shape::Channel {
        h: 0.16,
        b: 0.065,
        tw: 0.0075,
        tf: 0.0105,
    };
    // the purlin sits on the rafter's top flange
    let z = 3.0 + 0.15 + 0.08;
    let pur = bar(v3(-3.0, 0.0, z), v3(3.0, 0.0, z), ch, false);
    let c = sound(ConnKind::PurlinCleat, &[raf, pur], &Params::default());
    assert_eq!(count(&c, Role::Cleat), 2);
    assert!(!c.bolts.is_empty());
}

#[test]
fn stair_stringer_on_a_landing_beam() {
    let landing = bar(v3(-1.0, 0.0, 1.5), v3(1.0, 0.0, 1.5), ipe300(), false);
    let ch = Shape::Channel {
        h: 0.2,
        b: 0.075,
        tw: 0.0085,
        tf: 0.0115,
    };
    let st = bar(v3(0.0, 3.0, 0.0), v3(0.0, 0.0, 1.5), ch, false);
    let c = sound(ConnKind::StairStringer, &[landing, st], &Params::default());
    assert!(c.bolts.len() >= 2);
    assert!(c.notes.iter().all(|n| !n.contains("nearly level")));
}

#[test]
fn sphere_node_of_a_dome() {
    let apex = v3(0.0, 0.0, 6.0);
    let mut ms = vec![];
    for i in 0..6 {
        let a = std::f64::consts::TAU * i as f64 / 6.0;
        ms.push(bar(
            v3(4.0 * a.cos(), 4.0 * a.sin(), 4.5),
            apex,
            pipe(0.0761, 0.004),
            false,
        ));
    }
    let c = sound(ConnKind::SphereNode, &ms, &Params::default());
    assert_eq!(count(&c, Role::Ball), 1);
    assert_eq!(count(&c, Role::Sleeve), 6);
    assert_eq!(c.trims.len(), 6);
    // the ball keeps the free angle between every pair of sleeves
    let ball = c.parts.iter().find(|p| p.role == Role::Ball).unwrap();
    assert!(ball.mark.starts_with("ball D"));
    let s = schedule([&c]);
    assert_eq!(s.bolt_count(), 6);
}

#[test]
fn every_bolt_keeps_catalogue_holes_and_lengths() {
    let cat = catalog();
    let c = sound(
        ConnKind::RigidEndPlate,
        &column_beam(0.0, ipe300()),
        &Params::default(),
    );
    for (_, b) in &c.bolts {
        let size = cat.bolt(b.size.d).unwrap();
        assert!((b.hole - size.hole).abs() < 1e-9);
        assert!(cat.lengths.iter().any(|l| (l - b.length).abs() < 1e-9));
        assert!(b.length >= cat.bolt_need(size, b.grip) - 1e-9);
    }
}

#[test]
fn schedules_group_bolts_welds_and_plates() {
    let a = sound(
        ConnKind::Pinned,
        &column_beam(0.0, ipe300()),
        &Params::default(),
    );
    let b = sound(
        ConnKind::Pinned,
        &column_beam(0.0, ipe300()),
        &Params::default(),
    );
    let s = schedule([&a, &b]);
    assert_eq!(s.bolt_count(), 2 * a.bolts.len());
    assert_eq!(s.plates.len(), 1);
    assert_eq!(s.plates[0].count, 2);
    assert!(s.weld_length() > 0.0 && s.plate_mass() > 0.0);
}

/// The detailing sheet keeps the constructive rules and says it is no design calculation.
#[test]
fn detailing_sheet_keeps_the_rules_and_computes_no_resistance() {
    use super::detail::{sheet, NOT_DESIGN};
    let ms = column_beam(0.0, ipe300());
    let p = Params::parse("bolt=M20;class=10.9").unwrap();
    let c = sound(ConnKind::RigidEndPlate, &ms, &p);
    let s = sheet(&c, &check(&c, &ms));
    assert!(s.rules_kept(), "{:?}", s.findings);
    assert_eq!(s.kind, "rigid_end_plate");
    assert_eq!(s.stiffeners, 4);
    assert_eq!(s.groups.iter().map(|g| g.count).sum::<usize>(), c.bolts.len());
    for g in &s.groups {
        assert_eq!(g.bolt, "M20 10.9");
        assert!(g.pitch.is_some_and(|x| x >= g.pitch_min - 1e-9), "{g:?}");
    }
    assert!(s.schedule.weld_length() > 0.0 && !s.schedule.plates.is_empty());
    assert!(s.note == NOT_DESIGN && NOT_DESIGN.contains("not a design calculation"));
    let text = s.lines().join("\n");
    assert!(text.contains("PL") && !text.contains("kN"), "{text}");
    // every connection type yields a sheet with every bolt in a group
    let pin = sound(ConnKind::Pinned, &ms, &Params::default());
    let s = sheet(&pin, &check(&pin, &ms));
    assert!(s.rules_kept() && s.groups.iter().map(|g| g.count).sum::<usize>() == pin.bolts.len());
}

#[test]
fn analysis_hook_flags_unchecked_nodes() {
    let c = sound(
        ConnKind::Pinned,
        &column_beam(0.0, ipe300()),
        &Params::default(),
    );
    assert_eq!(verdict(&c), Verdict::NotChecked);
    assert_eq!(verdict(&c).key(), "not_checked");
}

#[test]
fn wrong_inputs_are_refused_with_reasons() {
    assert!(build(
        ConnKind::Pinned,
        &[col(0.0, 0.0, 0.0, 3.0, heb200())],
        &Params::default()
    )
    .is_err());
    let a = bar(v3(0.0, 0.0, 3.0), v3(3.0, 0.0, 3.0), ipe300(), false);
    let b = bar(v3(3.0, 0.0, 3.0), v3(3.0, 3.0, 3.0), ipe300(), false);
    let e = build(ConnKind::BeamSplice, &[a, b], &Params::default()).unwrap_err();
    assert!(e.0.contains("in line"));
}

#[test]
fn oversize_holes_follow_the_execution_table() {
    // clearance over the bolt diameter for oversized holes, mm
    for (name, add) in [
        ("M12", 3.0),
        ("M16", 4.0),
        ("M20", 4.0),
        ("M22", 4.0),
        ("M24", 6.0),
        ("M27", 8.0),
        ("M30", 8.0),
        ("M36", 8.0),
    ] {
        let b = catalog()
            .bolts
            .iter()
            .find(|b| b.name() == name)
            .unwrap_or_else(|| panic!("{name}"));
        assert!(
            (b.oversize - b.d - add / 1000.0).abs() < 1e-9,
            "{name}: {} mm",
            b.oversize * 1000.0
        );
    }
}

#[test]
fn asked_moment_rows_that_do_not_fit_are_refused() {
    let ms = column_beam(0.0, ipe300());
    let p = Params::parse("rows=9").unwrap();
    let e = build(ConnKind::RigidEndPlate, &ms, &p).unwrap_err();
    assert!(e.to_string().contains("bolt rows"), "{e}");
    let ok = Params::parse("rows=2").unwrap();
    assert!(build(ConnKind::RigidEndPlate, &ms, &ok).is_ok());
}
