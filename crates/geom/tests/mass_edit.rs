//! Direct modeling on masses: region push/pull, inferred distances, edge-set blends with vertex
//! blends, draft. Exact volumes, closed bodies, booleans afterwards, clear errors.

use std::f64::consts::PI;

use ddc_geom::csg::{ftag, Body};
use ddc_geom::mass::*;
use ddc_geom::{v2, v3, Tol, V3};

const T: Tol = Tol {
    length: 1e-7,
    angle: 1e-9,
};

fn closed(b: &Body) {
    b.validate(T)
        .unwrap_or_else(|e| panic!("not closed: {e:?}"));
}

fn unit() -> Body {
    cuboid(v3(0., 0., 0.), v3(1., 1., 1.))
}

fn sq(x0: f64, y0: f64, x1: f64, y1: f64, z: f64) -> Vec<V3> {
    vec![v3(x0, y0, z), v3(x1, y0, z), v3(x1, y1, z), v3(x0, y1, z)]
}

fn near(a: f64, b: f64, eps: f64) {
    assert!((a - b).abs() < eps, "{a} vs {b}");
}

/// Booleans keep working on the result.
fn boolean_ok(b: &Body) {
    let bb = b.bbox().unwrap();
    let c = (bb.0 + bb.1) * 0.5;
    let cyl = cylinder(v2(c.x, c.y), 0.05, bb.0.z - 1.0, bb.1.z - bb.0.z + 2.0, 24);
    let r = b.subtract(&cyl, T);
    closed(&r);
    assert!(r.volume() < b.volume());
}

#[test]
fn model3_push_region_boss_pocket_hole_imprint() {
    let r = sq(0.2, 0.2, 0.6, 0.6, 1.0);
    let boss = push_region(&unit(), 1, &r, 0.5, 9, T).unwrap();
    closed(&boss);
    near(boss.volume(), 1.08, 1e-9);
    near(boss.tag_area(ftag(9, 1)), 0.16, 1e-9);
    near(boss.tag_area(1), 0.84, 1e-9);
    boolean_ok(&boss);

    let pocket = push_region(&unit(), 1, &r, -0.3, 9, T).unwrap();
    closed(&pocket);
    near(pocket.volume(), 1.0 - 0.048, 1e-9);
    near(pocket.tag_area(ftag(9, 1)), 0.16, 1e-9);

    let d = push_distance(&unit(), 1, PushTarget::Through, T).unwrap();
    near(d, -1.0, 1e-12);
    let hole = push_region(&unit(), 1, &r, d, 9, T).unwrap();
    closed(&hole);
    near(hole.volume(), 0.84, 1e-9);

    let imp = push_region(&unit(), 1, &r, 0.0, 9, T).unwrap();
    closed(&imp);
    near(imp.volume(), 1.0, 1e-12);
    near(imp.tag_area(ftag(9, 1)), 0.16, 1e-9);
    near(imp.tag_area(1), 0.84, 1e-9);
    // the imprinted face is a face like any other: it pushes on its own
    let up = push_pull(&imp, ftag(9, 1), 0.25, 10, T).unwrap();
    closed(&up);
    near(up.volume(), 1.04, 1e-9);
}

#[test]
fn model3_push_region_from_edge_to_edge_continues_neighbour_faces() {
    // half of the top face, touching three of its edges
    let r = sq(0.0, 0.0, 0.5, 1.0, 1.0);
    let b = push_region(&unit(), 1, &r, 0.2, 9, T).unwrap();
    closed(&b);
    near(b.volume(), 1.1, 1e-9);
    // x = 0 side (tag 5) and y = 0 side (tag 2) grow, no new face on their planes
    near(b.tag_area(5), 1.2, 1e-9);
    near(b.tag_area(2), 1.1, 1e-9);
    near(b.tag_area(1), 0.5, 1e-9);
}

#[test]
fn model3_push_region_rejects_bad_loops() {
    let out = sq(0.8, 0.8, 1.2, 1.2, 1.0);
    assert_eq!(
        push_region(&unit(), 1, &out, 0.3, 9, T),
        Err(MassError::RegionOutside)
    );
    let off = sq(0.2, 0.2, 0.6, 0.6, 1.1);
    assert_eq!(
        push_region(&unit(), 1, &off, 0.3, 9, T),
        Err(MassError::BadParam("region loop is not on the face"))
    );
    let bow = vec![
        v3(0.2, 0.2, 1.0),
        v3(0.6, 0.6, 1.0),
        v3(0.6, 0.2, 1.0),
        v3(0.2, 0.6, 1.0),
    ];
    assert_eq!(
        push_region(&unit(), 1, &bow, 0.3, 9, T),
        Err(MassError::BadParam("region loop crosses itself"))
    );
    assert_eq!(
        push_region(&unit(), 1, &out[..2], 0.3, 9, T),
        Err(MassError::BadParam("region loop"))
    );
}

#[test]
fn model3_push_distance_infers_from_faces_points_and_through() {
    let a = unit().tagged(1);
    let c = cuboid(v3(2., 0., 0.), v3(3., 1., 2.)).tagged(2);
    let b = a.union(&c, T);
    let top = ftag(1, 1);
    let d = push_distance(&b, top, PushTarget::Face(ftag(2, 1)), T).unwrap();
    near(d, 1.0, 1e-12);
    let up = push_pull(&b, top, d, 3, T).unwrap();
    closed(&up);
    near(up.volume(), 4.0, 1e-9);
    near(
        push_distance(&b, top, PushTarget::Point(v3(5., 5., 1.75)), T).unwrap(),
        0.75,
        1e-12,
    );
    near(
        push_distance(&b, top, PushTarget::Through, T).unwrap(),
        -1.0,
        1e-12,
    );
    assert_eq!(
        push_distance(&b, top, PushTarget::Face(ftag(2, 2)), T),
        Err(MassError::NotParallel(top, ftag(2, 2)))
    );
    assert!(push_distance(&b, top, PushTarget::Face(top), T).is_err());
}

/// Rounded box: all twelve edges, eight spherical corners.
fn rounded_box(a: f64, b: f64, c: f64, r: f64) -> f64 {
    let edges = 4.0 * ((a - 2. * r) + (b - 2. * r) + (c - 2. * r));
    a * b * c - edges * r * r * (1. - PI / 4.) - 8. * r * r * r * (1. - PI / 6.)
}

const ALL12: [(u32, u32); 12] = [
    (0, 2),
    (0, 3),
    (0, 4),
    (0, 5),
    (1, 2),
    (1, 3),
    (1, 4),
    (1, 5),
    (2, 3),
    (3, 4),
    (4, 5),
    (5, 2),
];

#[test]
fn model3_fillet_all_edges_gives_rounded_box() {
    let bx = cuboid(v3(0., 0., 0.), v3(2., 1.5, 1.));
    let want = rounded_box(2., 1.5, 1., 0.25);
    let mut errs = vec![];
    for seg in [16, 32] {
        let b = blend_edges(&bx, &ALL12, BlendKind::Fillet { r: 0.25, seg }, 9, T).unwrap();
        closed(&b);
        // inscribed arcs remove a little more than the true rounds
        assert!(b.volume() < want, "{seg}: {} {want}", b.volume());
        errs.push(want - b.volume());
        // eight vertex blends, each an eighth of a sphere
        for k in 0..8 {
            let s = b.tag_area(ftag(9, 2 + 36 + k));
            near(s, PI * 0.25 * 0.25 / 2.0, 2e-3);
        }
        boolean_ok(&b);
    }
    assert!(errs[0] < 2e-3, "{errs:?}");
    // second-order convergence of the polygonal arcs
    assert!(errs[1] < errs[0] / 3.0, "{errs:?}");
}

#[test]
fn model3_fillet_three_edges_at_a_corner_is_a_ball_corner() {
    let r = 0.3;
    let b = blend_edges(
        &unit(),
        &[(1, 3), (1, 4), (3, 4)],
        BlendKind::Fillet { r, seg: 48 },
        9,
        T,
    )
    .unwrap();
    closed(&b);
    let want = 1. - 3. * (1. - r) * r * r * (1. - PI / 4.) - r * r * r * (1. - PI / 6.);
    near(b.volume(), want, 3e-4);
    near(b.tag_area(ftag(9, 2 + 9)), PI * r * r / 2.0, 2e-3);
    boolean_ok(&b);
}

#[test]
fn model3_fillet_two_edges_meet_in_a_mitre() {
    let r = 0.3;
    let b = blend_edges(
        &unit(),
        &[(1, 2), (1, 3)],
        BlendKind::Fillet { r, seg: 48 },
        9,
        T,
    )
    .unwrap();
    closed(&b);
    // two quarter-round strips minus their overlap at the shared vertex
    let want = 1. - 2. * r * r * (1. - PI / 4.) + r * r * r * (5. / 3. - PI / 2.);
    near(b.volume(), want, 3e-4);
}

#[test]
fn model3_chamfer_three_edges_cuts_a_flat_corner() {
    let d = 0.2;
    let b = blend_edges(
        &unit(),
        &[(1, 3), (1, 4), (3, 4)],
        BlendKind::Chamfer { d },
        9,
        T,
    )
    .unwrap();
    closed(&b);
    let s = 0.5f64.sqrt();
    let k = 1.0 / 3f64.sqrt();
    let want = convex_from_planes(
        &[
            (0, v3(0., 0., -1.), 0.),
            (1, v3(0., 0., 1.), 1.),
            (2, v3(0., -1., 0.), 0.),
            (3, v3(1., 0., 0.), 1.),
            (4, v3(0., 1., 0.), 1.),
            (5, v3(-1., 0., 0.), 0.),
            (6, v3(s, 0., s), (2. - d) * s),
            (7, v3(0., s, s), (2. - d) * s),
            (8, v3(s, s, 0.), (2. - d) * s),
            (9, v3(k, k, k), (3. - 2. * d) * k),
        ],
        T,
    )
    .unwrap();
    near(b.volume(), want.volume(), 1e-9);
    near(b.tag_area(ftag(9, 2 + 9)), 3f64.sqrt() / 2. * d * d, 1e-9);
    boolean_ok(&b);
}

fn l_body(h: f64) -> Body {
    let ring = [
        v2(0., 0.),
        v2(2., 0.),
        v2(2., 1.),
        v2(1., 1.),
        v2(1., 2.),
        v2(0., 2.),
    ];
    extrude(&ring, 0., h)
}

#[test]
fn model3_concave_edges_are_filled() {
    let b = l_body(1.0);
    let r = 0.2;
    let one = blend_edges(&b, &[(4, 5)], BlendKind::Fillet { r, seg: 32 }, 9, T).unwrap();
    closed(&one);
    near(one.volume(), 3. + r * r * (1. - PI / 4.), 1e-4);
    let e = edge_between(&b, 4, 5, 1e-7).unwrap();
    let old = fillet(&b, &e, r, 32, 9, T).unwrap();
    near(one.volume(), old.volume(), 1e-9);
    // concave vertical edge plus the convex top edges around it, one step
    let mix = blend_edges(
        &b,
        &[(4, 5), (1, 2)],
        BlendKind::Chamfer { d: 0.1 },
        9,
        T,
    )
    .unwrap();
    closed(&mix);
    near(mix.volume(), 3. + 0.1 * 0.1 / 2. - 0.1 * 0.1 / 2. * 2., 1e-9);
}

#[test]
fn model3_blend_errors_are_clear() {
    // two parallel top edges cannot hold more than half the width
    match blend_edges(
        &unit(),
        &[(1, 2), (1, 4)],
        BlendKind::Fillet { r: 0.6, seg: 8 },
        9,
        T,
    ) {
        Err(MassError::TooLarge { value, max }) => {
            near(value, 0.6, 0.0 + 1e-12);
            assert!(max > 0.49 && max <= 0.5 + 1e-6, "{max}");
        }
        e => panic!("{e:?}"),
    }
    match blend_edges(&unit(), &[(1, 2)], BlendKind::Chamfer { d: 1.5 }, 9, T) {
        Err(MassError::TooLarge { max, .. }) => near(max, 1.0, 1e-6),
        e => panic!("{e:?}"),
    }
    let msg = blend_edges(&unit(), &[(1, 2)], BlendKind::Fillet { r: 1.5, seg: 8 }, 9, T)
        .unwrap_err()
        .to_string();
    assert!(msg.contains("too large"), "{msg}");
    assert_eq!(
        blend_edges(&unit(), &[(1, 2), (2, 1)], BlendKind::Chamfer { d: 0.1 }, 9, T),
        Err(MassError::BadParam("edge listed twice"))
    );
    assert_eq!(
        blend_edges(&unit(), &[(0, 1)], BlendKind::Chamfer { d: 0.1 }, 9, T),
        Err(MassError::Coplanar)
    );
    let far = cuboid(v3(5., 5., 5.), v3(6., 6., 6.)).tagged(2);
    let two = unit().tagged(1).union(&far, T);
    assert_eq!(
        blend_edges(&two, &[(ftag(1, 1), ftag(2, 2))], BlendKind::Chamfer { d: 0.1 }, 9, T),
        Err(MassError::NoEdge(ftag(1, 1), ftag(2, 2)))
    );
    assert_eq!(
        blend_edges(&unit(), &[(1, 2)], BlendKind::Fillet { r: 0.0, seg: 8 }, 9, T),
        Err(MassError::BadParam("fillet radius"))
    );
    assert_eq!(
        blend_edges(&unit(), &[], BlendKind::Chamfer { d: 0.1 }, 9, T),
        Err(MassError::BadParam("no edges"))
    );
    // three of the four edges into a pyramid apex do not go around it
    let msg = blend_edges(
        &pyramid(),
        &[(1, 3), (3, 2), (2, 4)],
        BlendKind::Fillet { r: 0.05, seg: 8 },
        9,
        T,
    )
    .unwrap_err()
    .to_string();
    assert!(msg.contains("pick every edge there"), "{msg}");
    // convex top edges and the concave inner edge of an L meet at one vertex
    let msg = blend_edges(
        &l_body(1.0),
        &[(4, 5), (1, 4), (1, 5)],
        BlendKind::Fillet { r: 0.1, seg: 8 },
        9,
        T,
    )
    .unwrap_err()
    .to_string();
    assert!(msg.contains("in separate steps"), "{msg}");
}

#[test]
fn model3_mixed_vertex_blends_in_the_suggested_separate_steps() {
    let b = l_body(1.0);
    let r = 0.1;
    // convex top edges around the inner corner first
    let one = blend_edges(&b, &[(1, 4), (1, 5)], BlendKind::Fillet { r, seg: 32 }, 9, T).unwrap();
    closed(&one);
    assert!(one.volume() < b.volume());
    // then the concave inner edge, now ending where the first blends begin
    let two = blend_edges(&one, &[(4, 5)], BlendKind::Fillet { r, seg: 32 }, 10, T).unwrap();
    closed(&two);
    let fill = r * r * (1. - PI / 4.) * (1. - r);
    near(two.volume() - one.volume(), fill, 1e-4);
    boolean_ok(&two);
    // the same with chamfers
    let one = blend_edges(&b, &[(1, 4), (1, 5)], BlendKind::Chamfer { d: r }, 9, T).unwrap();
    let two = blend_edges(&one, &[(4, 5)], BlendKind::Chamfer { d: r }, 10, T).unwrap();
    closed(&two);
    near(two.volume() - one.volume(), r * r / 2. * (1. - r), 1e-9);
}

/// Square pyramid, base [-1, 1]^2 at z = 0, apex (0, 0, 1); sides 1..4, base 0.
fn pyramid() -> Body {
    let s = 0.5f64.sqrt();
    convex_from_planes(
        &[
            (0, v3(0., 0., -1.), 0.),
            (1, v3(s, 0., s), s),
            (2, v3(-s, 0., s), s),
            (3, v3(0., s, s), s),
            (4, v3(0., -s, s), s),
        ],
        T,
    )
    .unwrap()
}

fn top_z(b: &Body) -> f64 {
    b.polys
        .iter()
        .flat_map(|p| p.pts.iter())
        .fold(f64::MIN, |m, p| m.max(p.z))
}

#[test]
fn model3_four_edges_at_an_apex_get_a_setback_blend() {
    let apex = [(1, 3), (3, 2), (2, 4), (4, 1)];
    let v0 = 4.0 / 3.0;
    let mut last = 0.0;
    for seg in [16, 32] {
        let b = blend_edges(&pyramid(), &apex, BlendKind::Fillet { r: 0.1, seg }, 9, T).unwrap();
        closed(&b);
        let v = b.volume();
        assert!(v < v0 - 1e-4 && v > v0 - 0.02, "{v}");
        assert!(top_z(&b) < 1.0 - 1e-3, "{}", top_z(&b));
        // the vertex patch carries its own tag after the four edge surfaces
        assert!(b.tag_area(ftag(9, 2 + 12)) > 0.0);
        if seg == 32 {
            near(v, last, 1e-3);
        }
        last = v;
        boolean_ok(&b);
    }
    let c = blend_edges(&pyramid(), &apex, BlendKind::Chamfer { d: 0.1 }, 9, T).unwrap();
    closed(&c);
    assert!(c.volume() < v0 && top_z(&c) < 1.0 - 1e-3);
    // a pyramid pocket: four concave edges into its bottom are filled around one patch
    let s = 0.5f64.sqrt();
    let pocket = convex_from_planes(
        &[
            (21, v3(s, 0., -s), 0.),
            (22, v3(-s, 0., -s), 0.),
            (23, v3(0., s, -s), 0.),
            (24, v3(0., -s, -s), 0.),
            (25, v3(0., 0., 1.), 2.),
        ],
        T,
    )
    .unwrap();
    let blk = cuboid(v3(-2., -2., -1.), v3(2., 2., 1.)).subtract(&pocket, T);
    closed(&blk);
    assert!(!blk.contains(v3(0., 0., 0.02)));
    let f = blend_edges(
        &blk,
        &[(21, 23), (23, 22), (22, 24), (24, 21)],
        BlendKind::Fillet { r: 0.1, seg: 16 },
        9,
        T,
    )
    .unwrap();
    closed(&f);
    assert!(f.volume() > blk.volume() + 1e-5);
    assert!(f.contains(v3(0., 0., 0.02)));
    boolean_ok(&f);
}

#[test]
fn model3_chamfer_three_concave_edges_fills_a_flat_corner() {
    let d = 0.2;
    let b = cuboid(v3(0., 0., 0.), v3(3., 3., 3.))
        .tagged(1)
        .subtract(&cuboid(v3(1., 1., 1.), v3(4., 4., 4.)).tagged(2), T);
    closed(&b);
    let (bot, y1, x1) = (ftag(2, 0), ftag(2, 2), ftag(2, 5));
    let c = blend_edges(&b, &[(bot, y1), (bot, x1), (y1, x1)], BlendKind::Chamfer { d }, 9, T)
        .unwrap();
    closed(&c);
    // what stays empty of the cavity is convex
    let s = 0.5f64.sqrt();
    let k = 1.0 / 3f64.sqrt();
    let kept = convex_from_planes(
        &[
            (0, v3(-1., 0., 0.), -1.),
            (1, v3(1., 0., 0.), 3.),
            (2, v3(0., -1., 0.), -1.),
            (3, v3(0., 1., 0.), 3.),
            (4, v3(0., 0., -1.), -1.),
            (5, v3(0., 0., 1.), 3.),
            (6, v3(-s, -s, 0.), -(2. + d) * s),
            (7, v3(0., -s, -s), -(2. + d) * s),
            (8, v3(-s, 0., -s), -(2. + d) * s),
            (9, v3(-k, -k, -k), -(3. + 2. * d) * k),
        ],
        T,
    )
    .unwrap();
    near(c.volume(), 27.0 - kept.volume(), 1e-9);
    near(c.tag_area(ftag(9, 2 + 9)), 3f64.sqrt() / 2. * d * d, 1e-9);
    boolean_ok(&c);
}

#[test]
fn model3_draft_box_sides_gives_frustum() {
    let b = cuboid(v3(0., 0., 0.), v3(2., 2., 3.));
    let a = 5f64.to_radians();
    let t = a.tan();
    let d = draft(&b, &[2, 3, 4, 5], v3(0., 0., 0.), v3(0., 0., 1.), a, 9, T).unwrap();
    closed(&d);
    near(d.volume(), 12. - 36. * t + 36. * t * t, 1e-9);
    for f in 0..6 {
        assert!(d.tag_area(f) > 0.1, "face {f}");
    }
    near(d.tag_area(1), (2. - 6. * t).powi(2), 1e-9);
    boolean_ok(&d);
    // one face about a neutral plane through its middle: in and out cancel
    let one = draft(&b, &[3], v3(0., 0., 1.5), v3(0., 0., 1.), a, 9, T).unwrap();
    closed(&one);
    near(one.volume(), 12.0, 1e-9);
    near(one.tag_area(1), 2. * (2. - 1.5 * t), 1e-9);
}

#[test]
fn model3_draft_non_convex_body_offsets_every_section() {
    let a = 4f64.to_radians();
    let t = a.tan();
    let b = l_body(3.0);
    let walls = [2, 3, 4, 5, 6, 7];
    let up = v3(0., 0., 1.);
    let inw = draft(&b, &walls, v3(0., 0., 0.), up, a, 9, T).unwrap();
    closed(&inw);
    // section at height z: the L offset inward by z tan a (mitred), area 3 - 8 d + 4 d^2
    near(inw.volume(), 9. - 36. * t + 36. * t * t, 1e-9);
    let out = draft(&b, &walls, v3(0., 0., 0.), up, -a, 9, T).unwrap();
    closed(&out);
    near(out.volume(), 9. + 36. * t + 36. * t * t, 1e-9);
    for f in walls {
        assert!(inw.tag_area(f) > 0.5 && out.tag_area(f) > 0.5, "face {f}");
    }
    boolean_ok(&inw);
}

#[test]
fn model3_draft_errors_are_clear() {
    let b = cuboid(v3(0., 0., 0.), v3(2., 2., 3.));
    let o = v3(0., 0., 0.);
    let up = v3(0., 0., 1.);
    assert_eq!(
        draft(&b, &[1], o, up, 0.1, 9, T),
        Err(MassError::BadParam("face is normal to the pull direction"))
    );
    assert_eq!(
        draft(&b, &[2], o, up, 0.0, 9, T),
        Err(MassError::BadParam("draft angle"))
    );
    assert_eq!(
        draft(&b, &[2], o, up, 89.5f64.to_radians(), 9, T),
        Err(MassError::BadParam("draft angle"))
    );
    assert_eq!(
        draft(&b, &[], o, up, 0.1, 9, T),
        Err(MassError::BadParam("no faces to draft"))
    );
    assert_eq!(
        draft(&b, &[2, 3, 4, 5], o, up, 40f64.to_radians(), 9, T),
        Err(MassError::FaceLost(1))
    );
}
