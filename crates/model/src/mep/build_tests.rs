// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Planned networks written by `mep_build`.
use super::*;

/// A planned pipe and a planned duct, each rising 0.03 m from its port before the first turn:
/// no elbow fits the rise, so its corner goes to the next leg; both ends stay put.
#[test]
fn a_planned_leg_too_short_for_its_elbow_gives_its_corner_to_the_next() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let pts = vec![(6.948, 1.65, 2.17), (6.948, 1.65, 2.2), (5.6115, 1.65, 2.2), (5.6115, 2.4, 2.2)];
    let pipe = RunSpec::new(0, l, Section::new(Domain::Pipe, Shape::Round { d: 0.1 }), pts.clone());
    let duct = RunSpec::new(0, l, Section::new(Domain::Duct, Shape::Round { d: 0.2 }), pts.clone());
    for (kind, spec) in [(SystemKind::ColdWater, pipe), (SystemKind::ExhaustAir, duct)] {
        assert!(spec.radius(false) > 0.03, "{kind:?}: the rise must be too short for the elbow");
        let b = MepBuild {
            systems: vec![(format!("{kind:?}"), kind)],
            runs: vec![(SysRef::New(0), spec)],
            ..MepBuild::default()
        };
        let built = m.mep_build(&b).unwrap_or_else(|e| panic!("{kind:?}: {e:?}"));
        let run = m.mep_run(built.runs[0]).unwrap();
        assert_eq!(run.points.first(), pts.first(), "{kind:?}");
        assert_eq!(run.points.last(), pts.last(), "{kind:?}");
        assert!(run.points.len() < pts.len(), "{kind:?}: {:?}", run.points);
    }
}
