// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::collections::BTreeMap;
use std::f64::consts::FRAC_PI_8;

use ddc_geom::gsolid::{self, Form, Frame, Path3, Section};
use ddc_geom::math::v3;

use super::build::{self, Opts};
use super::tests::{hist, ring, tilted};
use super::*;

fn rel(a: f64, b: f64) -> f64 {
    (a - b).abs() / b.abs().max(1e-300)
}

pub(crate) fn rect(w: f64, h: f64) -> ddc_geom::arc::BulgeRing {
    ring(&[(-w / 2.0, -h / 2.0, 0.0), (w / 2.0, -h / 2.0, 0.0), (w / 2.0, h / 2.0, 0.0), (-w / 2.0, h / 2.0, 0.0)])
}

pub(crate) fn circle(r: f64) -> ddc_geom::arc::BulgeRing {
    ring(&[(r, 0.0, 1.0), (-r, 0.0, 1.0)])
}

/// Straight pieces with a mitre: a rectangle with a round hole along an L.
pub(crate) fn l_sweep() -> Form {
    Form::Sweep {
        profile: Section {
            outer: rect(0.3, 0.2),
            holes: vec![circle(0.05)],
        },
        path: Path3::polyline(vec![v3(0.0, 0.0, 0.0), v3(2.0, 0.0, 0.0), v3(2.0, 1.5, 0.5)]),
    }
}

/// Line, tangent quarter arc, line: a pipe bend.
pub(crate) fn bend() -> Form {
    Form::Sweep {
        profile: Section {
            outer: circle(0.1),
            holes: vec![],
        },
        path: Path3 {
            pts: vec![v3(0.0, 0.0, 0.0), v3(2.0, 0.0, 0.0), v3(3.0, 1.0, 0.0), v3(3.0, 3.0, 0.0)],
            bulges: vec![0.0, FRAC_PI_8.tan(), 0.0],
            smooth: None,
        },
    }
}

/// A quarter of a helix: fitted faces.
pub(crate) fn helix() -> Form {
    Form::Sweep {
        profile: Section {
            outer: rect(0.1, 0.08),
            holes: vec![],
        },
        path: Path3 {
            pts: vec![v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.5)],
            bulges: vec![FRAC_PI_8.tan()],
            smooth: None,
        },
    }
}

pub(crate) fn square_to_circle() -> Form {
    Form::Blend {
        bottom: rect(2.0, 2.0),
        top: circle(0.6),
        height: 1.5,
    }
}

fn exact(f: &Form, fr: &Frame) -> Body {
    let (b, why) = build::form_with(f, fr, &Opts::default()).unwrap();
    assert!(why.is_none(), "{why:?}");
    assert!(b.closed);
    b.check().unwrap();
    b
}

fn kinds(b: &Body) -> BTreeMap<FaceKind, usize> {
    b.histogram()
}

#[test]
fn mitred_sweep_is_planes_and_ruled_faces() {
    let f = l_sweep();
    let b = exact(&f, &tilted());
    // 4 sides x 2 pieces, 2 caps; the hole: 2 arcs x 2 pieces
    assert_eq!(kinds(&b), hist(&[(FaceKind::Plane, 10), (FaceKind::Nurbs, 4)]));
    let v = volume(&b).unwrap();
    let q = gsolid::quantities(&f).unwrap().volume;
    assert!(rel(v, q) < 1e-6, "{v} vs {q}");
}

#[test]
fn bend_is_exact_revolution() {
    let f = bend();
    let b = exact(&f, &Frame::default());
    assert_eq!(kinds(&b), hist(&[(FaceKind::Plane, 2), (FaceKind::Nurbs, 6)]));
    let v = volume(&b).unwrap();
    let q = gsolid::quantities(&f).unwrap().volume;
    assert!(rel(v, q) < 1e-5, "{v} vs {q}");
}

#[test]
fn helix_faces_are_fitted_within_tolerance() {
    let f = helix();
    for tol in [1e-4, 1e-6] {
        let (b, why) = build::form_with(&f, &Frame::default(), &Opts { tol, ..Opts::default() }).unwrap();
        assert!(why.is_none(), "{why:?}");
        b.check().unwrap();
        assert_eq!(kinds(&b), hist(&[(FaceKind::Plane, 2), (FaceKind::Nurbs, 4)]));
        let v = volume(&b).unwrap();
        let q = gsolid::quantities(&f).unwrap().volume;
        assert!(rel(v, q) < 1e-3, "tol {tol}: {v} vs {q}");
    }
}

#[test]
fn closed_loop_has_no_caps() {
    let f = Form::Sweep {
        profile: Section {
            outer: rect(0.2, 0.1),
            holes: vec![],
        },
        path: Path3::polyline(vec![
            v3(0.0, 0.0, 0.0),
            v3(2.0, 0.0, 0.0),
            v3(2.0, 2.0, 0.0),
            v3(0.0, 2.0, 0.0),
            v3(0.0, 0.0, 0.0),
        ]),
    };
    let b = exact(&f, &Frame::default());
    assert_eq!(kinds(&b), hist(&[(FaceKind::Plane, 16)]));
    let v = volume(&b).unwrap();
    let q = gsolid::quantities(&f).unwrap().volume;
    assert!(rel(v, q) < 1e-6, "{v} vs {q}");
}

#[test]
fn corner_next_to_an_arc_falls_back_with_a_reason() {
    let f = Form::Sweep {
        profile: Section {
            outer: rect(0.05, 0.05),
            holes: vec![],
        },
        path: Path3 {
            pts: vec![v3(0.0, 0.0, 0.0), v3(2.0, 0.0, 0.0), v3(3.0, 1.0, 0.0)],
            bulges: vec![0.0, -FRAC_PI_8.tan()],
            smooth: None,
        },
    };
    let (b, why) = build::form_with(&f, &Frame::default(), &Opts::default()).unwrap();
    assert!(why.unwrap().contains("corner"));
    assert!(b.faces.iter().all(|x| matches!(x.surface, Surface::Plane(_))));
}

#[test]
fn frustum_blend_is_planar() {
    let f = Form::Blend {
        bottom: rect(2.0, 2.0),
        top: rect(1.0, 1.0),
        height: 1.0,
    };
    let b = exact(&f, &tilted());
    assert_eq!(kinds(&b), hist(&[(FaceKind::Plane, 6)]));
    let v = volume(&b).unwrap();
    assert!(rel(v, 7.0 / 3.0) < 1e-9, "{v}");
}

#[test]
fn square_to_circle_blend_is_ruled() {
    let f = square_to_circle();
    let b = exact(&f, &Frame::default());
    let h = kinds(&b);
    assert_eq!(h.get(&FaceKind::Plane), Some(&2));
    assert!(h.get(&FaceKind::Nurbs).copied().unwrap_or(0) >= 4, "{h:?}");
    let v = volume(&b).unwrap();
    let q = gsolid::quantities(&f).unwrap().volume;
    // the pairing of arc and line points follows the curve parameters, not the arc length
    assert!(rel(v, q) < 1e-2, "{v} vs {q}");
}

#[test]
fn swept_blend_straight_and_curved() {
    let straight = Form::SweptBlend {
        bottom: rect(0.4, 0.4),
        top: rect(0.2, 0.2),
        path: Path3::polyline(vec![v3(0.0, 0.0, 0.0), v3(0.0, 3.0, 1.0)]),
    };
    let b = exact(&straight, &Frame::default());
    assert_eq!(kinds(&b), hist(&[(FaceKind::Plane, 6)]));
    let q = gsolid::quantities(&straight).unwrap().volume;
    assert!(rel(volume(&b).unwrap(), q) < 1e-3);
    let curved = Form::SweptBlend {
        bottom: rect(0.4, 0.4),
        top: rect(0.2, 0.2),
        path: Path3 {
            pts: vec![v3(0.0, 0.0, 0.0), v3(2.0, 2.0, 0.0)],
            bulges: vec![FRAC_PI_8.tan()],
            smooth: None,
        },
    };
    let b = exact(&curved, &tilted());
    assert_eq!(kinds(&b), hist(&[(FaceKind::Plane, 2), (FaceKind::Nurbs, 4)]));
    let q = gsolid::quantities(&curved).unwrap().volume;
    let v = volume(&b).unwrap();
    assert!(rel(v, q) < 2e-3, "{v} vs {q}");
}
