// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The checker itself: it must catch a hole too close to an edge, holes too close together,
//! a wrong hole size and two bolts in one place, or a clean result would prove nothing.

use super::catalog::catalog;
use super::check::{check, Rule};
use super::kit::rect;
use super::part::{bolt_set, plate, Role};
use super::tests::{bar, col, heb200, ipe300, sound};
use super::*;
use crate::gsolid::Frame;
use crate::math::{v2, v3};

fn conn_with(holes: Vec<(crate::math::V2, f64)>) -> Conn {
    Conn {
        kind: ConnKind::Pinned,
        node: v3(0.0, 0.0, 0.0),
        parts: vec![plate(
            Role::Plate,
            Frame::default(),
            rect(0.0, 0.0, 0.2, 0.2),
            holes,
            0.01,
            0.0,
        )],
        bolts: vec![],
        notes: vec![],
        trims: vec![],
    }
}

fn rules(c: &Conn) -> Vec<Rule> {
    check(c, &[]).into_iter().map(|f| f.rule).collect()
}

#[test]
fn checker_catches_edge_pitch_and_hole_faults() {
    let d0 = 0.022;
    let ok = conn_with(vec![(v2(-0.04, 0.0), d0), (v2(0.04, 0.0), d0)]);
    assert!(
        !rules(&ok).iter().any(|r| r.is_error()),
        "{:?}",
        check(&ok, &[])
    );
    let edge = conn_with(vec![(v2(0.09, 0.0), d0)]);
    assert!(rules(&edge).contains(&Rule::Edge));
    let pitch = conn_with(vec![(v2(0.0, 0.0), d0), (v2(0.03, 0.0), d0)]);
    assert!(rules(&pitch).contains(&Rule::Pitch));
    let mut hole = ok.clone();
    let m20 = catalog().bolt(0.02).unwrap().clone();
    let (parts, mut info) = bolt_set(
        &m20,
        "8.8",
        v3(-0.04, 0.0, 0.01),
        v3(0.0, 0.0, -1.0),
        0.01,
        0.024,
        v3(1.0, 0.0, 0.0),
    )
    .unwrap();
    info.plies = vec![0];
    hole.parts.extend(parts);
    hole.bolts.push((3, info));
    assert!(rules(&hole).contains(&Rule::Hole));
}

#[test]
fn checker_catches_bolts_in_one_place_and_heads_in_members() {
    let m20 = catalog().bolt(0.02).unwrap().clone();
    let mut c = conn_with(vec![(v2(-0.04, 0.0), 0.022), (v2(-0.03, 0.0), 0.022)]);
    for x in [-0.04, -0.03] {
        let (parts, mut info) = bolt_set(
            &m20,
            "8.8",
            v3(x, 0.0, 0.01),
            v3(0.0, 0.0, -1.0),
            0.01,
            0.022,
            v3(1.0, 0.0, 0.0),
        )
        .unwrap();
        info.plies = vec![0];
        let at = c.parts.len() + 2;
        c.parts.extend(parts);
        c.bolts.push((at, info));
    }
    assert!(rules(&c).contains(&Rule::Clash));
    // a column standing where the nuts are
    let column = col(-0.04, 0.0, -1.0, -0.005, heb200());
    let found = check(&c, &[column]);
    assert!(found
        .iter()
        .any(|f| f.rule == Rule::Clash && f.text.contains("member")));
}

#[test]
fn every_type_on_fixtures_has_no_clash() {
    let column = col(0.0, 0.0, 0.0, 4.0, heb200());
    let beam = bar(v3(0.0, 4.0, 3.0), v3(0.0, 0.0, 3.0), ipe300(), false);
    for kind in [ConnKind::Pinned, ConnKind::RigidEndPlate] {
        let ms = [column.clone(), beam.clone()];
        let c = build(kind, &ms, &Params::default()).unwrap();
        let clashes = check(&c, &ms)
            .into_iter()
            .filter(|f| f.rule == Rule::Clash)
            .count();
        assert_eq!(clashes, 0, "{}", kind.key());
    }
}

#[test]
fn gauge_across_the_plate_needs_the_wider_minimum() {
    let d0 = 0.022;
    // 50 mm: over the pitch minimum (48.4) but under the gauge minimum (52.8)
    let along = conn_with(vec![(v2(0.0, -0.025), d0), (v2(0.0, 0.025), d0)]);
    assert!(!rules(&along).contains(&Rule::Pitch));
    let across = conn_with(vec![(v2(-0.025, 0.0), d0), (v2(0.025, 0.0), d0)]);
    assert!(rules(&across).contains(&Rule::Pitch));
}

fn angle(s: f64, t: f64) -> crate::member::Shape {
    crate::member::Shape::Angle { h: s, b: s, t }
}

#[test]
fn end_plate_bolts_keep_the_pitch_round_the_corners() {
    // box legs wide enough for bolt rows on every side: the side rows meet the corner bolts
    let tube = crate::member::Shape::Tube { h: 0.4, b: 0.4, t: 0.016 };
    let lo = col(0.0, 0.0, 0.0, 3.0, tube.clone());
    let hi = col(0.0, 0.0, 3.0, 6.0, tube);
    let c = sound(ConnKind::ColumnSplice, &[lo, hi], &Params::default());
    assert!(c.bolts.len() >= 8, "{} bolts", c.bolts.len());
    let lo = col(0.0, 0.0, 0.0, 3.0, angle(0.15, 0.015));
    let hi = col(0.0, 0.0, 3.0, 6.0, angle(0.15, 0.015));
    sound(ConnKind::ColumnSplice, &[lo, hi], &Params::default());
}

#[test]
fn angle_web_bolts_clear_the_outstanding_leg() {
    // lattice node: angle braces lapping the gusset, the bolt line on the leg away from the heel
    let column = col(0.0, 0.0, 0.0, 4.0, heb200());
    for s in [(0.07, 0.007), (0.1, 0.01)] {
        let brace = bar(v3(0.0, 0.0, 2.0), v3(0.0, 2.5, 0.0), angle(s.0, s.1), false);
        sound(ConnKind::BracingGusset, &[column.clone(), brace], &Params::default());
    }
    let chord = bar(v3(-3.0, 0.0, 0.0), v3(3.0, 0.0, 0.0), angle(0.12, 0.012), false);
    let d1 = bar(v3(0.0, 0.0, 0.0), v3(-1.5, 0.0, 1.8), angle(0.07, 0.007), false);
    let d2 = bar(v3(0.0, 0.0, 0.0), v3(1.5, 0.0, 1.8), angle(0.07, 0.007), false);
    sound(ConnKind::TrussGusset, &[chord, d1, d2], &Params::default());
}

#[test]
fn braces_at_a_sliver_angle_are_refused_not_given_a_huge_gusset() {
    let column = col(0.0, 0.0, 0.0, 4.0, heb200());
    let a = bar(v3(0.0, 0.0, 2.0), v3(0.0, 3.0, 0.0), angle(0.07, 0.007), false);
    let b = bar(v3(0.0, 0.0, 2.0), v3(0.0, 3.0, -0.03), angle(0.07, 0.007), false);
    let e = build(ConnKind::BracingGusset, &[column, a, b], &Params::default()).unwrap_err();
    assert!(e.to_string().contains("angle"), "{e}");
}

#[test]
fn inclined_column_base_keeps_the_nuts_clear() {
    let leg = bar(v3(0.0, 0.0, 0.0), v3(0.0, 1.2, 4.0), heb200(), true);
    sound(ConnKind::BasePlate, &[leg], &Params::default());
    let leg = bar(v3(0.0, 0.0, 0.0), v3(1.2, 0.0, 4.0), heb200(), true);
    sound(ConnKind::BasePlate, &[leg], &Params::default());
}

#[test]
fn web_angle_rolled_to_the_gusset_keeps_heads_off_its_sloping_faces() {
    // the gusset plane cuts the L web obliquely: under a head the leg face slopes away
    let chord = bar(v3(1.3211, 0.9023, 2.4422), v3(0.0, 0.0, 0.0), angle(0.12, 0.012), false);
    let web = bar(v3(2.8211, 0.9023, 2.4422), v3(0.0, 0.0, 0.0), angle(0.07, 0.007), false);
    sound(ConnKind::TrussGusset, &[chord, web], &Params::default());
}
