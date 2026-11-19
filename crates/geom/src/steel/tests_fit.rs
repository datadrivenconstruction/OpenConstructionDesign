// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The checker itself: it must catch a hole too close to an edge, holes too close together,
//! a wrong hole size and two bolts in one place, or a clean result would prove nothing.

use super::catalog::catalog;
use super::check::{check, Rule};
use super::kit::rect;
use super::part::{bolt_set, plate, Role};
use super::tests::{bar, col, heb200, ipe300};
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
