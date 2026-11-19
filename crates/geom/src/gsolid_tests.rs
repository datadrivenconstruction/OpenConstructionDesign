// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<V2> {
    vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]
}

/// Circle of radius r centered at c: two semicircles (bulge = 1).
fn circle(c: V2, r: f64) -> BulgeRing {
    BulgeRing {
        pts: vec![c + v2(r, 0.0), c + v2(-r, 0.0)],
        bulges: vec![1.0, 1.0],
    }
}

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * b.abs().max(1e-12)
}

#[test]
fn ring_moments_exact_for_circle_and_polygon() {
    let (a, s) = ring_moments(&circle(v2(2.0, 1.0), 0.5));
    assert!(close(a, PI * 0.25, 1e-12), "{a}");
    assert!(close(s.x / a, 2.0, 1e-12) && close(s.y / a, 1.0, 1e-12));
    let (a, s) = ring_moments(&BulgeRing::from_polygon(rect(0.0, 0.0, 4.0, 2.0)));
    assert!(close(a, 8.0, 1e-12));
    assert!(close(s.x / a, 2.0, 1e-12) && close(s.y / a, 1.0, 1e-12));
    // semicircle: centroid at 4r/3π from the diameter
    let half = BulgeRing {
        pts: vec![v2(1.0, 0.0), v2(-1.0, 0.0)],
        bulges: vec![1.0, 0.0],
    };
    let (a, s) = ring_moments(&half);
    assert!(close(a, PI / 2.0, 1e-12));
    assert!(close(s.y / a, 4.0 / (3.0 * PI), 1e-12), "{}", s.y / a);
}

#[test]
fn extrusion_box_volume_area_and_watertight_mesh() {
    let f = Form::Extrusion {
        profile: Section::polygon(rect(0.0, 0.0, 2.0, 3.0)),
        start: 0.5,
        end: 4.5,
    };
    let q = quantities(&f).unwrap();
    assert!(close(q.volume, 24.0, 1e-12));
    assert!(close(q.area, 2.0 * 6.0 + 10.0 * 4.0, 1e-12));
    let m = mesh(&f, &Frame::default(), VIEW_SAG).unwrap();
    assert!(m.is_watertight());
    assert!(close(m.volume(), 24.0, 1e-9));
    let (lo, hi) = bbox(&m);
    assert!(close(lo.z, 0.5, 1e-12) && close(hi.z, 4.5, 1e-12));
}

#[test]
fn extrusion_with_hole_and_arcs() {
    let f = Form::Extrusion {
        profile: Section {
            outer: BulgeRing::from_polygon(rect(-2.0, -2.0, 2.0, 2.0)),
            holes: vec![circle(v2(0.0, 0.0), 1.0)],
        },
        start: 0.0,
        end: 2.0,
    };
    let q = quantities(&f).unwrap();
    assert!(close(q.volume, (16.0 - PI) * 2.0, 1e-12));
    let lateral = (16.0 + 2.0 * PI) * 2.0;
    assert!(close(q.area, 2.0 * (16.0 - PI) + lateral, 1e-12));
    let m = mesh(&f, &Frame::default(), 1e-4).unwrap();
    assert!(m.is_watertight());
    assert!(close(m.volume(), q.volume, 1e-4));
}

#[test]
fn mirrored_frame_keeps_outward_mesh() {
    let f = Form::Extrusion {
        profile: Section::polygon(rect(0.0, 0.0, 1.0, 1.0)),
        start: 0.0,
        end: 1.0,
    };
    let fr = Frame {
        x: v3(-1.0, 0.0, 0.0),
        ..Frame::default()
    };
    let m = mesh(&f, &fr, VIEW_SAG).unwrap();
    assert!(m.volume() > 0.0 && close(m.volume(), 1.0, 1e-12));
}

#[test]
fn revolve_torus_and_partial_cylinder_match_pappus() {
    // torus: circle r = 0.5 at distance R = 2 from the y axis
    let f = Form::Revolve {
        profile: Section {
            outer: circle(v2(2.0, 0.0), 0.5),
            holes: vec![],
        },
        axis_o: v2(0.0, 0.0),
        axis_d: v2(0.0, 1.0),
        start: 0.0,
        end: 2.0 * PI,
    };
    let q = quantities(&f).unwrap();
    assert!(
        close(q.volume, 2.0 * PI * PI * 2.0 * 0.25, 1e-12),
        "{}",
        q.volume
    );
    assert!(
        close(q.area, 4.0 * PI * PI * 2.0 * 0.5, 1e-12),
        "{}",
        q.area
    );
    let m = mesh(&f, &Frame::default(), 1e-4).unwrap();
    assert!(m.is_watertight());
    assert!(close(m.volume(), q.volume, 2e-3));
    // quarter of a hollow cylinder: rectangle [1, 2] x [0, 3] about the y axis, 90°
    let f = Form::Revolve {
        profile: Section::polygon(rect(1.0, 0.0, 2.0, 3.0)),
        axis_o: v2(0.0, 0.0),
        axis_d: v2(0.0, 1.0),
        start: 0.0,
        end: PI / 2.0,
    };
    let q = quantities(&f).unwrap();
    let v = PI / 4.0 * (4.0 - 1.0) * 3.0;
    assert!(close(q.volume, v, 1e-12));
    let m = mesh(&f, &Frame::default(), 1e-4).unwrap();
    assert!(m.is_watertight());
    assert!(close(m.volume(), v, 1e-3));
}

#[test]
fn revolve_rejects_axis_crossing_profile() {
    let f = Form::Revolve {
        profile: Section::polygon(rect(-1.0, 0.0, 1.0, 1.0)),
        axis_o: v2(0.0, 0.0),
        axis_d: v2(0.0, 1.0),
        start: 0.0,
        end: PI,
    };
    assert_eq!(validate(&f), Err(FormError::Axis));
    let f = Form::Revolve {
        profile: Section::polygon(rect(1.0, 0.0, 2.0, 1.0)),
        axis_o: v2(0.0, 0.0),
        axis_d: v2(0.0, 1.0),
        start: 0.0,
        end: 7.0,
    };
    assert_eq!(validate(&f), Err(FormError::Angle));
}

#[test]
fn blend_frustum_volume_is_prismatoid() {
    // frustum 2x2 -> 1x1, h = 3: V = h/3 (A0 + A1 + √(A0 A1))
    let f = Form::Blend {
        bottom: BulgeRing::from_polygon(rect(-1.0, -1.0, 1.0, 1.0)),
        top: BulgeRing::from_polygon(rect(-0.5, -0.5, 0.5, 0.5)),
        height: 3.0,
    };
    let q = quantities(&f).unwrap();
    assert!(
        close(q.volume, 1.0 * (4.0 + 1.0 + 2.0), 1e-12),
        "{}",
        q.volume
    );
    let m = mesh(&f, &Frame::default(), VIEW_SAG).unwrap();
    assert!(close(m.volume(), 7.0, 1e-9));
    // area: 4 trapezoids with slant height √(3² + 0.5²)
    let side = 4.0 * (2.0 + 1.0) / 2.0 * (9.0f64 + 0.25).sqrt();
    assert!(close(q.area, side + 5.0, 1e-6), "{}", q.area);
}

#[test]
fn sweep_straight_and_mitered_l_path() {
    let prof = Section::polygon(rect(-0.1, -0.2, 0.1, 0.2));
    let f = Form::Sweep {
        profile: prof.clone(),
        path: Path3::polyline(vec![v3(0.0, 0.0, 0.0), v3(5.0, 0.0, 0.0)]),
    };
    let q = quantities(&f).unwrap();
    assert!(close(q.volume, 0.08 * 5.0, 1e-12));
    assert!(close(q.area, 1.2 * 5.0 + 0.16, 1e-12));
    // L-shaped path: volume = A x centerline length (profile center on the path)
    let f = Form::Sweep {
        profile: prof,
        path: Path3::polyline(vec![
            v3(0.0, 0.0, 0.0),
            v3(4.0, 0.0, 0.0),
            v3(4.0, 3.0, 0.0),
        ]),
    };
    let q = quantities(&f).unwrap();
    assert!(close(q.volume, 0.08 * 7.0, 1e-12), "{}", q.volume);
    let m = mesh(&f, &Frame::default(), VIEW_SAG).unwrap();
    assert!(m.is_watertight());
    assert!(close(m.volume(), 0.08 * 7.0, 1e-9));
}

#[test]
fn sweep_along_arc_matches_pappus() {
    // quarter circle R = 3 in plan, round profile r = 0.2: V = A·R·θ
    let b = (PI / 8.0).tan();
    let f = Form::Sweep {
        profile: Section {
            outer: circle(v2(0.0, 0.0), 0.2),
            holes: vec![],
        },
        path: Path3 {
            pts: vec![v3(3.0, 0.0, 0.0), v3(0.0, 3.0, 0.0)],
            bulges: vec![b],
            smooth: None,
        },
    };
    let q = quantities(&f).unwrap();
    let v = PI * 0.04 * 3.0 * PI / 2.0;
    assert!(close(q.volume, v, 1e-6), "{} vs {v}", q.volume);
    let m = mesh(&f, &Frame::default(), 1e-4).unwrap();
    assert!(m.is_watertight());
    assert!(close(m.volume(), v, 2e-3));
}

#[test]
fn sweep_detects_fold_at_sharp_corner() {
    // profile wider than the turn radius toward the inside of a sharp corner
    let f = Form::Sweep {
        profile: Section::polygon(rect(-2.0, -0.1, 2.0, 0.1)),
        path: Path3::polyline(vec![
            v3(0.0, 0.0, 0.0),
            v3(1.0, 0.0, 0.0),
            v3(0.0, 0.3, 0.0),
        ]),
    };
    assert!(matches!(validate(&f), Err(FormError::SelfIntersect(_))));
}

#[test]
fn swept_blend_cone_along_line() {
    // square 2x2 -> 1x1 along a straight path of length 3: the same prismatoid as the blend
    let f = Form::SweptBlend {
        bottom: BulgeRing::from_polygon(rect(-1.0, -1.0, 1.0, 1.0)),
        top: BulgeRing::from_polygon(rect(-0.5, -0.5, 0.5, 0.5)),
        path: Path3::polyline(vec![v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 3.0)]),
    };
    let q = quantities(&f).unwrap();
    assert!(close(q.volume, 7.0, 1e-9), "{}", q.volume);
}

#[test]
fn profile_self_intersection_rejected() {
    let bow = vec![v2(0.0, 0.0), v2(1.0, 1.0), v2(1.0, 0.0), v2(0.0, 1.0)];
    let f = Form::Extrusion {
        profile: Section::polygon(bow),
        start: 0.0,
        end: 1.0,
    };
    assert!(matches!(validate(&f), Err(FormError::Profile(0, _))));
    let f = Form::Extrusion {
        profile: Section::polygon(rect(0.0, 0.0, 1.0, 1.0)),
        start: 1.0,
        end: 1.0,
    };
    assert_eq!(validate(&f), Err(FormError::Depth));
}

#[test]
fn clip_volume_box_in_box_and_cylinder() {
    let f = Form::Extrusion {
        profile: Section::polygon(rect(0.0, 0.0, 2.0, 2.0)),
        start: 0.0,
        end: 2.0,
    };
    let m = mesh(&f, &Frame::default(), VIEW_SAG).unwrap();
    // half of the cube in x, a quarter in z
    let v = clip_volume(&m, &prism_halves(&rect(-1.0, -1.0, 1.0, 3.0), 1.5, 5.0));
    assert!(close(v, 1.0 * 2.0 * 0.5, 1e-5), "{v}");
    // cutter entirely inside
    let v = clip_volume(&m, &prism_halves(&rect(0.5, 0.5, 1.5, 1.5), 0.5, 1.5));
    assert!(close(v, 1.0, 1e-5), "{v}");
    // cylinder r = 0.5 passing through: intersection with the strip prism y ∈ [-1, 0]
    let c = Form::Extrusion {
        profile: Section {
            outer: circle(v2(0.0, 0.0), 0.5),
            holes: vec![],
        },
        start: 0.0,
        end: 1.0,
    };
    let cm = mesh(&c, &Frame::default(), 1e-5).unwrap();
    let v = clip_volume(&cm, &prism_halves(&rect(-1.0, -1.0, 1.0, 0.0), -1.0, 2.0));
    assert!(close(v, PI * 0.25 / 2.0, 1e-4), "{v}");
}

#[test]
fn coplanar_faces_do_not_double_count() {
    let f = Form::Extrusion {
        profile: Section::polygon(rect(0.0, 0.0, 1.0, 1.0)),
        start: 0.0,
        end: 1.0,
    };
    let m = mesh(&f, &Frame::default(), VIEW_SAG).unwrap();
    // cutter coincides exactly with the solid
    let v = clip_volume(&m, &prism_halves(&rect(0.0, 0.0, 1.0, 1.0), 0.0, 1.0));
    assert!(close(v, 1.0, 1e-5), "{v}");
    // cutter touches by a face: zero volume
    let v = clip_volume(&m, &prism_halves(&rect(1.0, 0.0, 2.0, 1.0), 0.0, 1.0));
    assert!(v.abs() < 1e-5, "{v}");
}

#[test]
fn intersect_volume_of_two_meshes() {
    let a = mesh(
        &Form::Extrusion {
            profile: Section::polygon(rect(0.0, 0.0, 2.0, 2.0)),
            start: 0.0,
            end: 2.0,
        },
        &Frame::default(),
        VIEW_SAG,
    )
    .unwrap();
    let b = mesh(
        &Form::Extrusion {
            profile: Section::polygon(rect(1.0, 1.0, 3.0, 3.0)),
            start: 1.0,
            end: 3.0,
        },
        &Frame::default(),
        VIEW_SAG,
    )
    .unwrap();
    let v = intersect_volume(&a, &b);
    assert!(close(v, 1.0, 1e-5), "{v}");
    let v2_ = intersect_volume(&b, &a);
    assert!(close(v2_, 1.0, 1e-5), "{v2_}");
    // torus entirely inside a large cube
    let t = mesh(
        &Form::Revolve {
            profile: Section {
                outer: circle(v2(1.0, 0.0), 0.3),
                holes: vec![],
            },
            axis_o: v2(0.0, 0.0),
            axis_d: v2(0.0, 1.0),
            start: 0.0,
            end: 2.0 * PI,
        },
        &Frame::default(),
        1e-3,
    )
    .unwrap();
    let big = mesh(
        &Form::Extrusion {
            profile: Section::polygon(rect(-5.0, -5.0, 5.0, 5.0)),
            start: -5.0,
            end: 5.0,
        },
        &Frame::default(),
        VIEW_SAG,
    )
    .unwrap();
    let vt = intersect_volume(&big, &t);
    assert!(close(vt, t.volume(), 1e-6), "{vt} vs {}", t.volume());
}

#[test]
fn prism_overlap_with_hole() {
    let m = mesh(
        &Form::Extrusion {
            profile: Section::polygon(rect(-3.0, -3.0, 3.0, 3.0)),
            start: 0.0,
            end: 1.0,
        },
        &Frame::default(),
        VIEW_SAG,
    )
    .unwrap();
    let rings = vec![rect(-1.0, -1.0, 1.0, 1.0), rect(-0.5, -0.5, 0.5, 0.5)];
    let v = prism_overlap(&m, &rings, 0.0, 2.0);
    assert!(close(v, 3.0, 1e-5), "{v}");
}

#[test]
fn plan_cut_and_lines_of_cylinder_on_vertical_plane() {
    // cylinder extruded along y (vertical work plane along x)
    let f = Form::Extrusion {
        profile: Section {
            outer: circle(v2(0.0, 1.0), 0.5),
            holes: vec![],
        },
        start: 0.0,
        end: 4.0,
    };
    let fr = Frame::vertical(v3(0.0, 0.0, 0.0), v2(1.0, 0.0));
    let m = mesh(&f, &fr, 1e-4).unwrap();
    let cut = plan_cut(&m, 1.0);
    assert_eq!(cut.len(), 1);
    let a = signed_area(&cut[0].0);
    // axial section: rectangle 1 x 4
    assert!(close(a, 4.0, 1e-3), "{a}");
    assert!(!plan_lines(&m, 1.0).is_empty());
    // cross-section at y = -2: a circle
    let sc = section_cut(&m, v2(10.0, -2.0), v2(-1.0, 0.0));
    assert_eq!(sc.len(), 1);
    assert!(close(signed_area(&sc[0].0), PI * 0.25, 1e-3));
}

#[test]
fn hollow_section_nests_hole() {
    let f = Form::Extrusion {
        profile: Section {
            outer: BulgeRing::from_polygon(rect(0.0, 0.0, 4.0, 4.0)),
            holes: vec![BulgeRing::from_polygon(rect(1.0, 1.0, 3.0, 3.0))],
        },
        start: 0.0,
        end: 3.0,
    };
    let m = mesh(&f, &Frame::default(), VIEW_SAG).unwrap();
    let cut = plan_cut(&m, 1.5);
    assert_eq!(cut.len(), 1);
    assert_eq!(cut[0].1.len(), 1);
    let net = signed_area(&cut[0].0) + signed_area(&cut[0].1[0]);
    assert!(close(net, 12.0, 1e-9), "{net}");
}

fn boxm(x0: f64, y0: f64, x1: f64, y1: f64, z0: f64, z1: f64) -> Mesh {
    let f = Form::Extrusion {
        profile: Section::polygon(rect(x0, y0, x1, y1)),
        start: z0,
        end: z1,
    };
    mesh(&f, &Frame::default(), VIEW_SAG).unwrap()
}

#[test]
fn boolean_volume_counts_overlapping_voids_once() {
    let host = vec![rect(0.0, 0.0, 10.0, 1.0)];
    let h = [Body::Prism {
        rings: &host,
        z0: 0.0,
        z1: 3.0,
    }];
    let a = boxm(1.0, -1.0, 3.0, 2.0, 1.0, 2.0);
    let b = boxm(2.0, -1.0, 4.0, 2.0, 1.0, 2.0);
    // union 3 x 1 x 1, overlap 1 x 1 x 1: not counted twice
    let v = boolean_volume(&h, &[], &[Body::Mesh(&a), Body::Mesh(&b)]);
    assert!(close(v, 3.0, 1e-9), "{v}");
    // opening 2.5..3.5 already removed: only the remainder is subtracted
    let op = vec![rect(2.5, -5.0, 3.5, 5.0)];
    let minus = [Body::Prism {
        rings: &op,
        z0: 0.0,
        z1: 2.5,
    }];
    let v = boolean_volume(&h, &minus, &[Body::Mesh(&a), Body::Mesh(&b)]);
    assert!(close(v, 2.0, 1e-9), "{v}");
    // void misses the host
    let far = boxm(20.0, 0.0, 21.0, 1.0, 0.0, 1.0);
    assert_eq!(boolean_volume(&h, &[], &[Body::Mesh(&far)]), 0.0);
}

#[test]
fn boolean_volume_treats_layer_rings_as_pieces_and_holes() {
    // two separate layer pieces (wall gap): both are cut
    let pieces = vec![rect(0.0, 0.0, 1.0, 1.0), rect(2.0, 0.0, 3.0, 1.0)];
    let h = [Body::Prism {
        rings: &pieces,
        z0: 0.0,
        z1: 1.0,
    }];
    let v = boxm(-1.0, -1.0, 4.0, 2.0, 0.0, 0.5);
    let got = boolean_volume(&h, &[], &[Body::Mesh(&v)]);
    assert!(close(got, 1.0, 1e-9), "{got}");
    // outer contour with a hole (slab): the hole is not cut
    let slab = vec![rect(0.0, 0.0, 4.0, 4.0), rect(1.0, 1.0, 3.0, 3.0)];
    let h = [Body::Prism {
        rings: &slab,
        z0: 0.0,
        z1: 1.0,
    }];
    let got = boolean_volume(&h, &[], &[Body::Mesh(&v)]);
    // void covers x∈[0,4], y∈[0,2] x 0.5 minus the hole x∈[1,3], y∈[1,2]
    assert!(close(got, (8.0 - 2.0) * 0.5, 1e-9), "{got}");
}

#[test]
fn boolean_volume_of_cylinder_void_in_wall() {
    let host = vec![rect(-2.0, -0.1, 2.0, 0.1)];
    let h = [Body::Prism {
        rings: &host,
        z0: 0.0,
        z1: 3.0,
    }];
    // horizontal cylinder r = 0.3 across the wall (axis along y)
    let f = Form::Extrusion {
        profile: Section {
            outer: circle(v2(0.0, 1.5), 0.3),
            holes: vec![],
        },
        start: -1.0,
        end: 1.0,
    };
    let m = mesh(&f, &Frame::vertical(v3(0.0, 0.0, 0.0), v2(1.0, 0.0)), 1e-5).unwrap();
    let exact = PI * 0.09 * 0.2;
    let got = boolean_volume(&h, &[], &[Body::Mesh(&m)]);
    assert!(close(got, exact, 1e-3), "{got} vs {exact}");
}

#[test]
fn through_area_only_for_full_depth() {
    let host = vec![rect(0.0, 0.0, 10.0, 10.0)];
    let h = [Body::Prism {
        rings: &host,
        z0: -0.2,
        z1: 0.0,
    }];
    let hole = boxm(1.0, 1.0, 3.0, 2.0, -1.0, 1.0);
    let recess = boxm(5.0, 5.0, 7.0, 7.0, -0.01, 1.0);
    let a = through_area(
        &h,
        &[],
        &[Body::Mesh(&hole), Body::Mesh(&recess)],
        -0.2,
        0.0,
    );
    assert!(close(a, 2.0, 1e-9), "{a}");
    // two partial cuts from the top and bottom, together passing through
    let top = boxm(5.0, 5.0, 6.0, 6.0, -0.1, 1.0);
    let bot = boxm(5.0, 5.0, 6.0, 6.0, -1.0, -0.1);
    let a = through_area(&h, &[], &[Body::Mesh(&top), Body::Mesh(&bot)], -0.2, 0.0);
    assert!(close(a, 1.0, 1e-9), "{a}");
}

#[test]
fn plan_lines_skip_smooth_facets_but_keep_silhouette() {
    // cylinder lying down: in plan two end circles and two silhouette lines, not every face
    let f = Form::Extrusion {
        profile: Section {
            outer: circle(v2(0.0, 1.0), 0.5),
            holes: vec![],
        },
        start: 0.0,
        end: 4.0,
    };
    let m = mesh(&f, &Frame::vertical(v3(0.0, 0.0, 0.0), v2(1.0, 0.0)), 1e-4).unwrap();
    let lines = plan_lines(&m, 10.0);
    let long = lines.iter().filter(|(a, b)| (*b - *a).len() > 3.9).count();
    assert_eq!(long, 2, "{lines:?}");
    // order is deterministic
    assert_eq!(plan_lines(&m, 10.0), lines);
}

#[test]
fn twisted_short_or_self_crossing_path_is_rejected() {
    let profile = Section::polygon(rect(-0.1, -0.1, 0.1, 0.1));
    // a planar closed triangle is a valid ring now
    let tri = Path3::polyline(vec![
        v3(0.0, 0.0, 0.0),
        v3(5.0, 0.0, 0.0),
        v3(5.0, 5.0, 0.0),
        v3(0.0, 0.0, 0.0),
    ]);
    let f = Form::Sweep {
        profile: profile.clone(),
        path: tri,
    };
    assert_eq!(validate(&f), Ok(()));
    // a loop whose frame comes back turned would leave a gap at the seam
    let twisted = Path3::polyline(vec![
        v3(0.0, 0.0, 0.0),
        v3(4.0, 0.0, 0.0),
        v3(4.0, 3.0, 1.0),
        v3(1.0, 5.0, 2.0),
        v3(-1.0, 2.0, 0.5),
        v3(0.0, 0.0, 0.0),
    ]);
    let f = Form::Sweep {
        profile: profile.clone(),
        path: twisted,
    };
    assert!(matches!(validate(&f), Err(FormError::Path(_))));
    // two distinct points cannot close a loop
    let back = Path3::polyline(vec![
        v3(0.0, 0.0, 0.0),
        v3(5.0, 0.0, 0.0),
        v3(0.0, 0.0, 0.0),
    ]);
    let f = Form::Sweep {
        profile: profile.clone(),
        path: back,
    };
    assert!(matches!(validate(&f), Err(FormError::Path(_))));
    let cross = Path3::polyline(vec![
        v3(0.0, 0.0, 0.0),
        v3(5.0, 0.0, 0.0),
        v3(5.0, 5.0, 0.0),
        v3(2.0, -3.0, 0.0),
    ]);
    let f = Form::Sweep {
        profile,
        path: cross,
    };
    assert!(matches!(validate(&f), Err(FormError::SelfIntersect(_))));
}

#[test]
fn clockwise_ring_keeps_first_point_as_vertex_zero() {
    let cw = BulgeRing::from_polygon(vec![v2(0.0, 0.0), v2(0.0, 1.0), v2(1.0, 1.0), v2(1.0, 0.0)]);
    let p = ccw_poly(&cw, VIEW_SAG);
    assert_eq!(p[0], v2(0.0, 0.0));
    assert!(signed_area(&p) > 0.0);
}

#[test]
fn swept_blend_on_straight_path_matches_blend_volume() {
    let bottom = BulgeRing::from_polygon(rect(-0.5, -0.5, 0.5, 0.5));
    let top = BulgeRing::from_polygon(rect(-0.25, -0.25, 0.25, 0.25));
    let sb = Form::SweptBlend {
        bottom: bottom.clone(),
        top: top.clone(),
        path: Path3::polyline(vec![v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 2.0)]),
    };
    let bl = Form::Blend {
        bottom,
        top,
        height: 2.0,
    };
    let (a, b) = (quantities(&sb).unwrap(), quantities(&bl).unwrap());
    assert!(
        close(a.volume, b.volume, 1e-3),
        "{} vs {}",
        a.volume,
        b.volume
    );
}

fn square_loop() -> Path3 {
    Path3::polyline(vec![
        v3(0.0, 0.0, 0.0),
        v3(4.0, 0.0, 0.0),
        v3(4.0, 4.0, 0.0),
        v3(0.0, 4.0, 0.0),
        v3(0.0, 0.0, 0.0),
    ])
}

fn near(a: V3, b: V3) -> bool {
    (a - b).len() < 1e-12
}

#[test]
fn closed_square_path_sweep_volume_exact() {
    let prof = Section::polygon(rect(-0.1, -0.2, 0.1, 0.2));
    let path = square_loop();
    let (st, closed) = path_stations(&path, QTY_SAG, &[]).unwrap();
    assert!(closed);
    // the seam point is one station, not two
    assert_eq!(st.len(), 4);
    for i in 0..4 {
        assert!(near(st[i].p, path.pts[i]));
        for j in i + 1..4 {
            assert!(!near(st[i].p, st[j].p), "stations {i} and {j} coincide");
        }
    }
    // every band, the seam band 3 -> 0 included, runs along its own segment: frames match
    for k in 0..4 {
        let seg = (path.pts[k + 1] - path.pts[k]).normalized();
        for q in rect(-0.1, -0.2, 0.1, 0.2) {
            let g = at_station(&st, (k + 1) % 4, q) - at_station(&st, k, q);
            assert!(g.cross(seg).len() < 1e-12, "band {k} at {q:?}");
            assert!(g.dot(seg) > 0.0);
        }
    }
    let f = Form::Sweep {
        profile: prof,
        path,
    };
    let q = quantities(&f).unwrap();
    assert!(close(q.volume, 0.08 * 16.0, 1e-12), "{}", q.volume);
    // sides 0.4 * (16.8 + 15.2), top and bottom 0.2 * 16 each, no end caps
    assert!(close(q.area, 19.2, 1e-12), "{}", q.area);
    let m = mesh(&f, &Frame::default(), VIEW_SAG).unwrap();
    assert!(m.is_watertight());
    assert!(close(m.volume(), 1.28, 1e-9), "{}", m.volume());
    assert!(close(m.area(), 19.2, 1e-9), "{}", m.area());
}

#[test]
fn closed_circle_sweep_torus_pappus_volume() {
    // circle R = 3 in plan from two half arcs, round profile r = 0.2: V = pi r^2 * 2 pi R
    let path = Path3 {
        pts: vec![v3(3.0, 0.0, 0.0), v3(-3.0, 0.0, 0.0), v3(3.0, 0.0, 0.0)],
        bulges: vec![1.0, 1.0],
        smooth: None,
    };
    let (st, closed) = path_stations(&path, QTY_SAG, &[]).unwrap();
    assert!(closed);
    assert!(!near(st[st.len() - 1].p, st[0].p));
    let f = Form::Sweep {
        profile: Section {
            outer: circle(v2(0.0, 0.0), 0.2),
            holes: vec![],
        },
        path,
    };
    let q = quantities(&f).unwrap();
    let v = PI * 0.04 * 2.0 * PI * 3.0;
    let a = 2.0 * PI * 0.2 * 2.0 * PI * 3.0;
    assert!(close(q.volume, v, 1e-6), "{} vs {v}", q.volume);
    assert!(close(q.area, a, 1e-6), "{} vs {a}", q.area);
    let m = mesh(&f, &Frame::default(), 1e-4).unwrap();
    assert!(m.is_watertight());
    assert!(close(m.volume(), v, 2e-3), "{} vs {v}", m.volume());
}

#[test]
fn closed_path_swept_blend_refused() {
    let bottom = BulgeRing::from_polygon(rect(-0.2, -0.2, 0.2, 0.2));
    let top = BulgeRing::from_polygon(rect(-0.1, -0.1, 0.1, 0.1));
    let f = Form::SweptBlend {
        bottom: bottom.clone(),
        top: top.clone(),
        path: square_loop(),
    };
    assert!(matches!(validate(&f), Err(FormError::Path(_))));
    assert!(matches!(
        mesh(&f, &Frame::default(), VIEW_SAG),
        Err(FormError::Path(_))
    ));
    assert!(matches!(quantities(&f), Err(FormError::Path(_))));
    // the same blend on the open square is fine
    let mut open = square_loop();
    open.pts.pop();
    open.bulges.pop();
    let f = Form::SweptBlend { bottom, top, path: open };
    assert_eq!(validate(&f), Ok(()));
}

#[test]
fn open_path_sweep_volume_unchanged() {
    // stations of an open L path, pinned by hand: one per point, end planes square to the path
    let path = Path3::polyline(vec![
        v3(0.0, 0.0, 0.0),
        v3(4.0, 0.0, 0.0),
        v3(4.0, 3.0, 0.0),
    ]);
    let (st, closed) = path_stations(&path, QTY_SAG, &[]).unwrap();
    assert!(!closed);
    assert_eq!(st.len(), 3);
    let (x, y, z) = (v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0), v3(0.0, 0.0, 1.0));
    let h = 0.5f64.sqrt();
    let want = [
        (v3(0.0, 0.0, 0.0), x, x, y, z, 0.0),
        (v3(4.0, 0.0, 0.0), x, v3(h, h, 0.0), y, z, 4.0),
        (v3(4.0, 3.0, 0.0), y, y, v3(-1.0, 0.0, 0.0), z, 7.0),
    ];
    for (s, (p, t, n, u, v, len)) in st.iter().zip(want) {
        assert!(near(s.p, p) && near(s.t, t) && near(s.n, n), "{s:?}");
        assert!(near(s.u, u) && near(s.v, v) && (s.s - len).abs() < 1e-12, "{s:?}");
    }
    // an end near the start but not on it keeps the path open, with end caps
    let almost = Path3::polyline(vec![
        v3(0.0, 0.0, 0.0),
        v3(4.0, 0.0, 0.0),
        v3(4.0, 4.0, 0.0),
        v3(0.0, 4.0, 0.0),
        v3(0.0, 0.5, 0.0),
    ]);
    let (st, closed) = path_stations(&almost, QTY_SAG, &[]).unwrap();
    assert!(!closed);
    assert_eq!(st.len(), 5);
    let prof = Section::polygon(rect(-0.1, -0.2, 0.1, 0.2));
    let f = Form::Sweep {
        profile: prof.clone(),
        path: almost,
    };
    let q = quantities(&f).unwrap();
    assert!(close(q.volume, 0.08 * 15.5, 1e-12), "{}", q.volume);
    let m = mesh(&f, &Frame::default(), VIEW_SAG).unwrap();
    assert!(m.is_watertight());
    assert!(close(m.volume(), 0.08 * 15.5, 1e-9));
    // straight and L sweeps keep their volumes and end caps
    let line = Form::Sweep {
        profile: prof.clone(),
        path: Path3::polyline(vec![v3(0.0, 0.0, 0.0), v3(5.0, 0.0, 0.0)]),
    };
    let q = quantities(&line).unwrap();
    assert!(close(q.volume, 0.4, 1e-12) && close(q.area, 6.16, 1e-12));
    let l = Form::Sweep { profile: prof, path };
    assert!(close(quantities(&l).unwrap().volume, 0.56, 1e-12));
}
