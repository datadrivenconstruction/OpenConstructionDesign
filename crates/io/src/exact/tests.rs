// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::collections::BTreeMap;
use std::f64::consts::{FRAC_PI_2, TAU};

use ddc_geom::arc::BulgeRing;
use ddc_geom::gsolid::{self, Form, Frame, Section};
use ddc_geom::math::{v2, v3, V2};
use ddc_geom::nurbs;

use super::build::{self, Seg};
use super::*;

pub(crate) fn ring(pts: &[(f64, f64, f64)]) -> BulgeRing {
    BulgeRing {
        pts: pts.iter().map(|p| v2(p.0, p.1)).collect(),
        bulges: pts.iter().map(|p| p.2).collect(),
    }
}

pub(crate) fn hist(pairs: &[(FaceKind, usize)]) -> BTreeMap<FaceKind, usize> {
    pairs.iter().copied().collect()
}

/// Rectangle with one arc side and a round hole.
pub(crate) fn slab() -> Form {
    Form::Extrusion {
        profile: Section {
            outer: ring(&[(0.0, 0.0, 0.0), (3.0, 0.0, 0.4), (3.0, 2.0, 0.0), (0.0, 2.0, 0.0)]),
            holes: vec![ring(&[(1.0, 1.0, 1.0), (1.8, 1.0, 1.0)])],
        },
        start: -0.2,
        end: 0.5,
    }
}

/// Full turn about x: disc, cylinders, a cone and a toroidal groove.
pub(crate) fn spindle() -> Form {
    Form::Revolve {
        profile: Section {
            outer: ring(&[
                (0.0, 0.0, 0.0),
                (4.0, 0.0, 0.0),
                (4.0, 1.0, 0.0),
                (3.0, 1.0, -0.5),
                (2.0, 1.0, 0.0),
                (1.0, 2.0, 0.0),
                (0.0, 2.0, 0.0),
            ]),
            holes: vec![],
        },
        axis_o: v2(0.0, 0.0),
        axis_d: v2(1.0, 0.0),
        start: 0.0,
        end: TAU,
    }
}

/// Quarter turn of a profile with a spherical end.
pub(crate) fn dome() -> Form {
    Form::Revolve {
        profile: Section {
            outer: ring(&[(0.0, 0.0, 0.0), (2.0, 0.0, (TAU / 16.0).tan()), (1.0, 1.0, 0.0), (0.0, 1.0, 0.0)]),
            holes: vec![],
        },
        axis_o: v2(0.0, 0.0),
        axis_d: v2(1.0, 0.0),
        start: 0.3,
        end: 0.3 + FRAC_PI_2,
    }
}

/// Full turn of a tube profile with a round hole: a toroidal void.
pub(crate) fn tube() -> Form {
    Form::Revolve {
        profile: Section {
            outer: ring(&[(1.0, 0.0, 0.0), (3.0, 0.0, 0.0), (3.0, 2.0, 0.0), (1.0, 2.0, 0.0)]),
            holes: vec![ring(&[(1.5, 1.0, 1.0), (2.5, 1.0, 1.0)])],
        },
        axis_o: v2(0.0, -1.0),
        axis_d: v2(0.0, 1.0),
        start: 0.0,
        end: TAU,
    }
}

pub(crate) fn tilted() -> Frame {
    let s = 0.5f64.sqrt();
    Frame {
        o: v3(10.0, -4.0, 2.5),
        x: v3(s, s, 0.0),
        y: v3(0.0, 0.0, 1.0),
        z: v3(s, -s, 0.0),
    }
}

fn rel(a: f64, b: f64) -> f64 {
    (a - b).abs() / b.abs().max(1e-300)
}

fn exact_volume(f: &Form, fr: &Frame) -> (Body, f64) {
    let (b, exact) = build::form(f, fr, 1e-3).unwrap();
    assert!(exact);
    b.check().unwrap();
    let m = recognize(&b);
    let v = m.volume().unwrap_or_else(|| panic!("not recognised: {m:?}"));
    (b, v)
}

#[test]
fn slab_faces_and_volume() {
    let f = slab();
    let (b, v) = exact_volume(&f, &tilted());
    assert_eq!(b.histogram(), hist(&[(FaceKind::Plane, 5), (FaceKind::Cylinder, 3)]));
    let q = gsolid::quantities(&f).unwrap().volume;
    assert!(rel(v, q) < 1e-9, "{v} vs {q}");
}

#[test]
fn spindle_faces_and_volume() {
    let f = spindle();
    let (b, v) = exact_volume(&f, &Frame::default());
    assert_eq!(
        b.histogram(),
        hist(&[
            (FaceKind::Plane, 2),
            (FaceKind::Cylinder, 2),
            (FaceKind::Cone, 1),
            (FaceKind::Torus, 1)
        ])
    );
    assert!(matches!(recognize(&b), Mapped::Revolve { .. }));
    let q = gsolid::quantities(&f).unwrap().volume;
    assert!(rel(v, q) < 1e-9, "{v} vs {q}");
}

#[test]
fn dome_partial_turn() {
    let f = dome();
    let (b, v) = exact_volume(&f, &tilted());
    assert_eq!(
        b.histogram(),
        hist(&[(FaceKind::Plane, 3), (FaceKind::Cylinder, 1), (FaceKind::Sphere, 1)])
    );
    let q = gsolid::quantities(&f).unwrap().volume;
    assert!(rel(v, q) < 1e-9, "{v} vs {q}");
}

#[test]
fn tube_has_a_void_shell() {
    let f = tube();
    let (b, v) = exact_volume(&f, &Frame::default());
    assert_eq!(b.shells.len(), 2);
    assert_eq!(
        b.histogram(),
        hist(&[(FaceKind::Plane, 2), (FaceKind::Cylinder, 2), (FaceKind::Torus, 2)])
    );
    let q = gsolid::quantities(&f).unwrap().volume;
    assert!(rel(v, q) < 1e-9, "{v} vs {q}");
}

#[test]
fn mirror_keeps_a_valid_body() {
    let (b, _) = build::form(&slab(), &Frame::default(), 1e-3).unwrap();
    let m = Xf {
        m: [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        t: v3(1.0, 2.0, 3.0),
    };
    let mb = b.transformed(&m).unwrap();
    mb.check().unwrap();
    let v = recognize(&mb).volume().unwrap();
    let q = gsolid::quantities(&slab()).unwrap().volume;
    assert!(rel(v, q) < 1e-9, "{v} vs {q}");
}

#[test]
fn spindle_torus_falls_back_to_nurbs() {
    // an outward bump whose centre lies inside the axis distance of its radius
    let f = Form::Revolve {
        profile: Section {
            outer: ring(&[(0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (1.0, 1.0, 0.3), (0.0, 1.0, 0.0)]),
            holes: vec![],
        },
        axis_o: v2(0.0, 0.0),
        axis_d: v2(1.0, 0.0),
        start: 0.0,
        end: TAU,
    };
    let (b, exact) = build::form(&f, &Frame::default(), 1e-3).unwrap();
    assert!(exact);
    b.check().unwrap();
    assert_eq!(b.histogram().get(&FaceKind::Nurbs), Some(&1));
}

#[test]
fn polyhedron_of_a_blend() {
    let f = Form::Blend {
        bottom: ring(&[(0.0, 0.0, 0.0), (2.0, 0.0, 0.0), (2.0, 2.0, 0.0), (0.0, 2.0, 0.0)]),
        top: ring(&[(0.5, 0.5, 0.0), (1.5, 0.5, 0.0), (1.5, 1.5, 0.0), (0.5, 1.5, 0.0)]),
        height: 1.0,
    };
    let m = gsolid::mesh(&f, &Frame::default(), 1e-3).unwrap();
    let b = build::polyhedron(&m).unwrap();
    assert!(b.closed);
    b.check().unwrap();
    let v = recognize(&b).volume().unwrap();
    let q = gsolid::quantities(&f).unwrap().volume;
    assert!(rel(v, q) < 1e-6, "{v} vs {q}");
}

#[test]
fn bulge_through_three_points() {
    for &bg in &[0.2, -0.2, 1.0, -1.0, 2.5, -2.5] {
        let s = Seg {
            a: v2(1.0, 0.5),
            b: v2(-0.4, 2.0),
            bulge: bg,
        };
        let m: V2 = s.mid();
        let got = Seg::bulge_through(s.a, m, s.b);
        assert!((got - bg).abs() < 1e-9, "{bg} -> {got}");
    }
}

#[test]
fn nurbs_sheet_has_four_sides_or_a_seam() {
    let line = nurbs::Curve::polyline(&[v3(1.0, 0.0, 0.0), v3(1.0, 0.0, 2.0)]).unwrap();
    let quarter = nurbs::revolve(&line, v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), FRAC_PI_2).unwrap();
    let b = nurbs_face::sheet(&quarter).unwrap();
    b.check().unwrap();
    assert!(!b.closed);
    assert_eq!(b.edges.len(), 4);
    let full = nurbs::revolve(&line, v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), TAU).unwrap();
    let b = nurbs_face::sheet(&full).unwrap();
    b.check().unwrap();
    assert_eq!(b.edges.len(), 3, "two rims and one seam");
    assert!(matches!(recognize(&b), Mapped::Sheets(ref s) if s.len() == 1));
}

#[test]
fn nurbs_shell_closes_with_offset_faces_and_ruled_sides() {
    let line = nurbs::Curve::polyline(&[v3(1.0, 0.0, 0.0), v3(1.0, 0.0, 2.0)]).unwrap();
    let quarter = nurbs::revolve(&line, v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), FRAC_PI_2).unwrap();
    let (b, dev) = nurbs_face::shell(&quarter, -0.05, 0.05, 5e-4).unwrap();
    b.check().unwrap();
    assert!(b.closed, "two offset faces and four sides close");
    assert_eq!(b.faces.len(), 6);
    assert!(b.faces.iter().all(|f| matches!(f.surface, Surface::Nurbs(_))));
    assert!(dev <= 5e-4, "{dev}");
    // a quarter annulus 0.1 thick and 2 high: faces point out when the volume is positive
    let want = FRAC_PI_2 / 2.0 * (1.05f64.powi(2) - 0.95f64.powi(2)) * 2.0;
    if let Some(v) = volume(&b) {
        assert!((v - want).abs() < 0.01 * want, "{v} vs {want}");
    }
    // a full turn: the seam takes no side face, its rulings pair up as seams too
    let full = nurbs::revolve(&line, v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), TAU).unwrap();
    let (b, _) = nurbs_face::shell(&full, 0.0, 0.1, 5e-4).unwrap();
    b.check().unwrap();
    assert!(b.closed);
    assert_eq!(b.faces.len(), 4, "inner, outer and two rims");
    assert!(nurbs_face::shell(&quarter, 0.1, 0.1, 5e-4).is_err(), "no thickness");
}

#[test]
fn thickened_quarter_cylinder_is_exact_to_its_volume() {
    let line = nurbs::Curve::polyline(&[v3(1.0, 0.0, 0.0), v3(1.0, 0.0, 2.0)]).unwrap();
    let quarter = nurbs::revolve(&line, v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), FRAC_PI_2).unwrap();
    let (b, dev) = nurbs_face::shell(&quarter, -0.05, 0.05, model::OFFSET_TOL).unwrap();
    b.check().unwrap();
    assert!(b.closed);
    assert!(dev < 1e-9, "scaled control net, not a refit: {dev}");
    for f in &b.faces[..2] {
        let Surface::Nurbs(s) = &f.surface else { panic!("not a NURBS face") };
        assert_eq!((s.nu(), s.nv()), (quarter.nu(), quarter.nv()), "same net as the patch");
    }
    let want = FRAC_PI_2 / 2.0 * (1.05f64.powi(2) - 0.95f64.powi(2)) * 2.0;
    let v = volume(&b).expect("B-rep volume");
    assert!(rel(v, want) < 1e-3, "{v} vs {want}");
}

#[test]
fn thickened_dome_keeps_its_pole_as_a_degenerate_edge_and_its_volume() {
    let (r, rise, t) = (5.0, 2.0, 0.12);
    let dome = nurbs::dome(v3(0.0, 0.0, 0.0), r, rise).unwrap();
    let (b, dev) = nurbs_face::shell(&dome, 0.0, t, model::OFFSET_TOL).unwrap();
    b.check().unwrap();
    assert!(b.closed);
    assert!(dev < 1e-9, "homothety about the centre: {dev}");
    assert!(b.faces.iter().all(|f| matches!(f.surface, Surface::Nurbs(_))));
    assert_eq!(b.edges.iter().filter(|e| e.degenerate()).count(), 2, "one pole per offset face");
    // two spherical sectors of half angle a, the rim face a cone through the centre
    let rho = (r * r + rise * rise) / (2.0 * rise);
    let cos_a = (rho - rise) / rho;
    let want = TAU / 3.0 * (1.0 - cos_a) * ((rho + t).powi(3) - rho.powi(3));
    let v = volume(&b).expect("B-rep volume");
    assert!(rel(v, want) < 1e-3, "{v} vs {want}");
}

#[test]
fn dome_freeform_goes_to_step_with_a_degenerate_pole_edge() {
    let dome = nurbs::dome(v3(0.0, 0.0, 0.0), 5.0, 2.0).unwrap();
    for t in [0.0, 0.12] {
        let m = freeform_model_of(dome.clone(), t, nurbs::Trim::default());
        let (d, rep) = model::doc(&m, &crate::meshx::MeshSet::default(), true, "ff", &build::Opts::default()).unwrap();
        assert_eq!((rep.exact, rep.faceted), (1, 0), "{:?}", rep.notes);
        let body = &d.parts[0].body;
        body.check().unwrap();
        assert!(body.edges.iter().any(|e| e.degenerate()), "the pole is an edge");
        assert!(body.faces.iter().all(|f| matches!(f.surface, Surface::Nurbs(_))), "no mesh");
        let (text, _) = step_text(&m);
        assert!(!text.contains("POLY_LOOP") && !text.contains("FACETED_BREP") && !text.contains("TODO"));
        assert_eq!(text.contains("MANIFOLD_SOLID_BREP"), t > 0.0);
        crate::ifc::check_step(&text).unwrap();
        let (back, _) = crate::step::read_exact(text.as_bytes()).unwrap();
        let b = &back.parts[0].body;
        b.check().unwrap();
        assert_eq!(b.closed, t > 0.0);
        assert_eq!(b.edges.iter().filter(|e| e.degenerate()).count(), body.edges.iter().filter(|e| e.degenerate()).count());
    }
}

fn bump() -> nurbs::Surface {
    let mut ctrl = vec![];
    for i in 0..3 {
        for j in 0..3 {
            let z = if i == 1 && j == 1 { 1.0 } else { 0.0 };
            ctrl.push(v3(2.0 * i as f64, 2.0 * j as f64, z));
        }
    }
    let k = vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    nurbs::Surface::new(2, 2, k.clone(), k, 3, 3, ctrl, vec![1.0; 9]).unwrap()
}

fn freeform_model(thickness: f64, trim: nurbs::Trim) -> ddc_model::Model {
    freeform_model_of(bump(), thickness, trim)
}

fn freeform_model_of(surface: nurbs::Surface, thickness: f64, trim: nurbs::Trim) -> ddc_model::Model {
    let mut m = ddc_model::Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 3.0).unwrap();
    m.add_freeform(&ddc_model::FreeformSpec {
        level: l,
        name: "canopy".into(),
        material: String::new(),
        surface,
        thickness,
        side: ddc_model::ShellSide::Normal,
        trim,
        origin: "points".into(),
    })
    .unwrap();
    m
}

fn step_text(m: &ddc_model::Model) -> (String, model::Report) {
    let o = build::Opts::default();
    let (d, rep) = model::doc(m, &crate::meshx::MeshSet::default(), true, "ff", &o).unwrap();
    let w = crate::step::write::WriteOptions {
        schema: crate::step::write::Schema::Ap214,
        units: crate::meshx::Units::Metre,
        stamp: String::new(),
    };
    (crate::step::write::write(&d, &w).unwrap(), rep)
}

#[test]
fn freeform_patch_goes_to_step_as_its_exact_nurbs_face() {
    let m = freeform_model(0.0, nurbs::Trim::default());
    let (d, _) = model::doc(&m, &crate::meshx::MeshSet::default(), true, "ff", &build::Opts::default()).unwrap();
    let face = &d.parts[0].body.faces[0];
    let Surface::Nurbs(s) = &face.surface else { panic!("not a NURBS face") };
    assert_eq!((s.nu(), s.nv()), (3, 3), "the stored control net, not a refit");
    assert!((s.ctrl()[4].z - 4.0).abs() < 1e-12, "lifted by the level elevation");
    let (text, rep) = step_text(&m);
    assert_eq!((rep.exact, rep.faceted), (1, 0), "{:?}", rep.notes);
    assert!(text.contains("B_SPLINE_SURFACE_WITH_KNOTS('',2,2,"), "polynomial 3 x 3 patch");
    assert!(text.contains("SHELL_BASED_SURFACE_MODEL"), "a bare surface is an open sheet");
    assert!(!text.contains("MANIFOLD_SOLID_BREP"));
}

#[test]
fn thickened_freeform_goes_to_step_as_a_closed_shell_and_a_trimmed_one_as_a_mesh() {
    let m = freeform_model(0.2, nurbs::Trim::default());
    let (text, rep) = step_text(&m);
    assert_eq!((rep.exact, rep.faceted), (1, 0), "{:?}", rep.notes);
    assert!(text.contains("CLOSED_SHELL") && text.contains("MANIFOLD_SOLID_BREP"));
    assert!(text.contains("B_SPLINE_SURFACE_WITH_KNOTS"));
    let trim = nurbs::Trim {
        outer: Some(vec![[0.1, 0.1], [0.9, 0.1], [0.9, 0.9], [0.1, 0.9]]),
        holes: vec![],
    };
    let (_, rep) = step_text(&freeform_model(0.0, trim));
    assert_eq!((rep.exact, rep.faceted), (0, 1));
    assert!(rep.notes[0].contains("trimmed"), "{:?}", rep.notes);
}

#[test]
fn add_solids_puts_freeform_patches_in_a_table_set() {
    let m = freeform_model(0.2, nurbs::Trim::default());
    let mut set = crate::meshx::MeshSet::default();
    model::add_solids(&m, &mut set).unwrap();
    let p = set.parts.iter().find(|p| p.key.starts_with("freeform:")).expect("freeform part");
    assert_eq!((p.category.as_str(), p.name.as_str()), ("freeform", "canopy"));
    assert!(!p.triangles.is_empty());
    // the set's freeform part is the one written exactly: no second copy
    let (d, rep) = model::doc(&m, &set, true, "ff", &build::Opts::default()).unwrap();
    assert_eq!((d.parts.len(), rep.exact), (1, 1));
}
