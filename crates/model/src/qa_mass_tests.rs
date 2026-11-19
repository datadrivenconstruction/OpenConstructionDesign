// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! QA: mass forms and cuts.

use crate::qa_methods_tests::{fx, rect};
use crate::{Form, MassSpec};

fn cuboid() -> Form {
    Form::Cuboid { lo: (0.0, 0.0, 0.0), hi: (2.0, 2.0, 2.0) }
}

#[test]
fn form_history_helpers() {
    let c = cuboid();
    assert_eq!(Form::kind(&c), "cuboid");
    assert_eq!(Form::op_kinds(&c), vec!["cuboid"]);
    assert_eq!(Form::root_op(&c), 1);
    let e = Form::Extrude { ring: rect(0.0, 0.0, 2.0, 2.0), z0: 0.0, h: 1.0, ids: vec![] };
    assert_eq!(Form::kind(&e), "extrude");
    let pp = Form::PushPull { base: Box::new(c.clone()), face: ddc_geom::csg::ftag(1, 1), d: 0.5, keys: vec![] };
    assert_eq!(Form::kind(&pp), "push_pull");
    assert_eq!(Form::op_kinds(&pp), vec!["cuboid", "push_pull"]);
    assert_eq!(Form::root_op(&pp), 2);
    let mv = Form::Move { base: Box::new(pp.clone()), d: (1.0, 0.0, 0.0) };
    assert_eq!(Form::op_kinds(&mv).len() as u32, Form::root_op(&mv));
    // the frozen history is the same shape and evaluates to the same body
    let fz = Form::frozen(&pp).unwrap();
    assert_eq!(Form::op_kinds(&fz), Form::op_kinds(&pp));
    assert!(Form::frozen(&Form::Cuboid { lo: (0., 0., 0.), hi: (0., 0., 0.) }).is_err());
}

#[test]
fn mass_next_op_and_cut_queries_without_cutters() {
    let mut f = fx();
    let id = f
        .m
        .add_mass(&MassSpec {
            level: f.l1,
            name: "M".into(),
            void: false,
            inplace: false,
            form: cuboid(),
        })
        .unwrap();
    assert_eq!(f.m.next_mass_op(id).unwrap(), 2);
    assert!(f.m.next_mass_op(9999).is_err());
    let w = f.walls[0];
    assert!(f.m.cut_section("wall", w, 1.0).unwrap().is_none(), "nothing cuts the wall");
    assert!(f.m.cut_mesh("wall", w).unwrap().is_none());
    assert!(f.m.cutter_bodies("wall", w).unwrap().is_empty());
    assert!(f.m.cut_section_layers("wall", w, 1.0).unwrap().is_none());
    assert!(f.m.joined_parts("wall", w).is_ok());
}
