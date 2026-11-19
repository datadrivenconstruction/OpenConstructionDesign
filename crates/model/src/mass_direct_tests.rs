// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Direct modeling steps on masses (docs/MODEL3.md): region push/pull to targets, edge-set
//! blends, draft. Each step is one undo step and survives the JSON history.

use std::f64::consts::PI;

use ddc_geom::csg::{ftag, BoolOp};

use crate::{Form, MassSpec, Model, PushGoal};

fn block() -> Form {
    Form::Extrude {
        ring: vec![(0., 0.), (10., 0.), (10., 6.), (0., 6.)],
        z0: 0.0,
        h: 9.0,
        ids: vec![],
    }
}

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L0", 0.0).unwrap();
    let id = m
        .add_mass(&MassSpec {
            level: lv,
            name: "block".into(),
            void: false,
            inplace: false,
            form: block(),
        })
        .unwrap();
    (m, id)
}

fn vol(m: &Model, id: i64) -> f64 {
    m.mass_body(id).unwrap().volume()
}

fn roundtrip(m: &Model, id: i64) {
    let f = m.mass(id).unwrap().form;
    let back = Form::from_json(&serde_json::from_str(&f.to_json().to_string()).unwrap()).unwrap();
    assert_eq!(back, f);
}

#[test]
fn model3_steps_are_single_undo_steps() {
    let (mut m, id) = model();
    let v0 = vol(&m, id);
    assert!((v0 - 540.0).abs() < 1e-9);
    let top = ftag(1, 1);
    // boss on a region of the roof, 3 m up
    m.edit_mass_form(id, |b| Form::PushTo {
        base: b,
        face: top,
        region: vec![(2., 2., 9.), (4., 2., 9.), (4., 4., 9.), (2., 4., 9.)],
        to: PushGoal::Dist(3.0),
        keys: vec![],
    })
    .unwrap();
    assert!((vol(&m, id) - 552.0).abs() < 1e-9);
    roundtrip(&m, id);
    m.undo().unwrap();
    assert!((vol(&m, id) - v0).abs() < 1e-9);

    // four vertical edges rounded in one step
    let r = 1.0;
    m.edit_mass_form(id, |b| Form::Blends {
        base: b,
        edges: vec![
            (ftag(1, 2), ftag(1, 3)),
            (ftag(1, 3), ftag(1, 4)),
            (ftag(1, 4), ftag(1, 5)),
            (ftag(1, 5), ftag(1, 2)),
        ],
        round: true,
        size: r,
        seg: 32,
    })
    .unwrap();
    let want = 540.0 - 4.0 * 9.0 * r * r * (1.0 - PI / 4.0);
    assert!((vol(&m, id) - want).abs() < 2e-2, "{}", vol(&m, id));
    roundtrip(&m, id);
    m.undo().unwrap();
    assert!((vol(&m, id) - v0).abs() < 1e-9);

    // draft of the four sides about the ground plane
    let a = 3f64.to_radians();
    let t = a.tan();
    m.edit_mass_form(id, |b| Form::Draft {
        base: b,
        faces: (2..6).map(|k| ftag(1, k)).collect(),
        at: (0., 0., 0.),
        pull: (0., 0., 1.),
        angle: a,
    })
    .unwrap();
    let want = 540.0 - 1296.0 * t + 972.0 * t * t;
    assert!((vol(&m, id) - want).abs() < 1e-9, "{}", vol(&m, id));
    roundtrip(&m, id);
    m.undo().unwrap();
    assert!((vol(&m, id) - v0).abs() < 1e-9);
}

#[test]
fn model3_failed_step_changes_nothing() {
    let (mut m, id) = model();
    let before = m.mass(id).unwrap().form;
    let e = m
        .edit_mass_form(id, |b| Form::Blends {
            base: b,
            edges: vec![(ftag(1, 1), ftag(1, 2)), (ftag(1, 1), ftag(1, 4))],
            round: true,
            size: 4.0,
            seg: 8,
        })
        .unwrap_err()
        .to_string();
    assert!(e.contains("too large") && e.contains("at most 3"), "{e}");
    assert_eq!(m.mass(id).unwrap().form, before);
    let e = m
        .edit_mass_form(id, |b| Form::PushTo {
            base: b,
            face: ftag(1, 1),
            region: vec![(8., 4., 9.), (12., 4., 9.), (12., 8., 9.)],
            to: PushGoal::Dist(1.0),
            keys: vec![],
        })
        .unwrap_err()
        .to_string();
    assert!(e.contains("region leaves the face"), "{e}");
    assert_eq!(m.mass(id).unwrap().form, before);
}

#[test]
fn model3_shell_hollows_with_removed_faces_in_one_step() {
    let (mut m, id) = model();
    let t = 0.5;
    // open roof: cavity 9 x 5 x 8.5
    m.edit_mass_form(id, |b| Form::Shell {
        base: b,
        t,
        open: vec![ftag(1, 1)],
        keys: vec![],
    })
    .unwrap();
    let b = m.mass_body(id).unwrap();
    b.validate(ddc_geom::Tol::default()).unwrap();
    assert!((b.volume() - 157.5).abs() < 1e-9, "{}", b.volume());
    roundtrip(&m, id);
    m.undo().unwrap();
    assert!((vol(&m, id) - 540.0).abs() < 1e-9);

    // closed shell of the block with a boss: the cavity follows the boss
    m.edit_mass_form(id, |b| Form::PushTo {
        base: b,
        face: ftag(1, 1),
        region: vec![(2., 2., 9.), (4., 2., 9.), (4., 4., 9.), (2., 4., 9.)],
        to: PushGoal::Dist(3.0),
        keys: vec![],
    })
    .unwrap();
    m.edit_mass_form(id, |b| Form::Shell {
        base: b,
        t,
        open: vec![],
        keys: vec![],
    })
    .unwrap();
    let b = m.mass_body(id).unwrap();
    b.validate(ddc_geom::Tol::default()).unwrap();
    // 552 - (9 x 5 x 8 + 1 x 1 x 3)
    assert!((b.volume() - 189.0).abs() < 1e-9, "{}", b.volume());
    m.undo().unwrap();
    assert!((vol(&m, id) - 552.0).abs() < 1e-9);

    let before = m.mass(id).unwrap().form;
    let e = m
        .edit_mass_form(id, |b| Form::Shell {
            base: b,
            t: 3.1,
            open: vec![],
            keys: vec![],
        })
        .unwrap_err()
        .to_string();
    assert!(e.contains("no cavity"), "{e}");
    assert_eq!(m.mass(id).unwrap().form, before);
}

#[test]
fn model3_push_to_a_face_follows_the_target() {
    let low = Form::Cuboid {
        lo: (0., 0., 0.),
        hi: (2., 2., 1.),
    };
    let high = |h: f64| Form::Cuboid {
        lo: (4., 0., 0.),
        hi: (6., 2., h),
    };
    let f = |h: f64| Form::PushTo {
        base: Form::Bool {
            op: BoolOp::Union,
            a: low.clone().base(),
            b: high(h).base(),
        }
        .base(),
        face: ftag(1, 1),
        region: vec![],
        to: PushGoal::Face(ftag(2, 1)),
        keys: vec![],
    };
    // lower block rises to the top of the higher one, wherever that is
    assert!((f(3.0).eval().unwrap().volume() - 24.0).abs() < 1e-9);
    assert!((f(5.0).eval().unwrap().volume() - 40.0).abs() < 1e-9);
    let through = Form::PushTo {
        base: low.clone().base(),
        face: ftag(1, 1),
        region: vec![(0.5, 0.5, 1.), (1.5, 0.5, 1.), (1.5, 1.5, 1.), (0.5, 1.5, 1.)],
        to: PushGoal::Through,
        keys: vec![],
    };
    let b = through.eval().unwrap();
    assert!((b.volume() - 3.0).abs() < 1e-9);
    let back =
        Form::from_json(&serde_json::from_str(&through.to_json().to_string()).unwrap()).unwrap();
    assert_eq!(back, through);
    assert_eq!(through.op_kinds(), vec!["cuboid", "push_to"]);
}
