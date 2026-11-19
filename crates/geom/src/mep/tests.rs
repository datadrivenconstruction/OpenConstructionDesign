// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::clash::{clashes, Body, Convex};
use super::*;

fn close(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

#[test]
fn steel_dn100_long_radius_elbow_length() {
    let s = PipeMaterial::Steel.by_dn(100).unwrap();
    assert_eq!(s.nps, Some("4"));
    assert!(close(s.od, 0.1143, 1e-12));
    let r = PipeMaterial::Steel.bend_radius(&s);
    assert!(close(r, 0.1524, 1e-12));
    let pts = [v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(1.0, 1.0, 0.0)];
    let ps = route(&pts, &|_| r, (0.0, 0.0)).unwrap();
    assert_eq!(ps.len(), 3);
    // 90° DN100 elbow: 152.4 · π/2 = 0.239389 m
    assert!(close(ps[1].length(), 0.1524 * PI / 2.0, 1e-12));
    assert!(close(ps[1].length(), 0.239389, 1e-6));
    let total: f64 = ps.iter().map(|p| p.length()).sum();
    assert!(close(total, 2.0 - 2.0 * 0.1524 + 0.1524 * PI / 2.0, 1e-12));
    // piece ends join up
    for w in ps.windows(2) {
        assert!((w[0].ends().1 - w[1].ends().0).len() < 1e-12);
    }
}

#[test]
fn nps_parsing_and_tables() {
    assert_eq!(nps_inches("1-1/4"), 1.25);
    assert_eq!(nps_inches("1/2"), 0.5);
    for m in PipeMaterial::ALL {
        let s = m.sizes();
        assert!(s.windows(2).all(|w| w[0].dn < w[1].dn && w[0].od < w[1].od));
        assert!(s.iter().all(|x| x.id() > 0.0));
    }
    assert_eq!(PipeMaterial::Steel.by_nps("2").unwrap().dn, 50);
    assert!(close(
        PipeMaterial::Pvc.by_dn(100).unwrap().od,
        0.110,
        1e-12
    ));
}

#[test]
fn duct_400x200_surface_per_metre() {
    let s = Section::new(Domain::Duct, Shape::Rect { w: 0.4, h: 0.2 });
    let q = qty_along(&s, 1.0);
    assert!(close(q.surface, 1.2, 1e-12));
    assert!(close(q.volume, 0.08, 1e-12));
    let mut ins = s;
    ins.insulation = 0.05;
    ins.wall = 0.001;
    let q = qty_along(&ins, 1.0);
    assert!(close(q.insulation, 0.502 * 0.302 - 0.402 * 0.202, 1e-12));
    assert!(close(q.material, 0.402 * 0.202 - 0.08, 1e-12));
}

#[test]
fn oval_and_round_formulas() {
    let o = Shape::Oval { w: 0.5, h: 0.2 };
    assert!(close(o.area(), 0.3 * 0.2 + PI * 0.01, 1e-12));
    assert!(close(o.perimeter(), 0.6 + PI * 0.2, 1e-12));
    let ring = o.outline(64);
    assert!(close(crate::poly::area(&ring), o.area(), 2e-3));
    let eq = o.outline_equal_area(16);
    assert!(close(crate::poly::area(&eq), o.area(), 1e-12));
}

#[test]
fn sanitary_two_percent_over_five_metres() {
    let p = apply_slope(
        &[v3(0.0, 0.0, 1.0), v3(3.0, 0.0, 0.0), v3(3.0, 2.0, 0.0)],
        2.0,
    );
    assert!(close(p[2].z, 0.9, 1e-12));
    assert!(close(p[0].z - p[2].z, 0.1, 1e-12));
}

#[test]
fn elbow_volume_is_pappus_and_mesh_agrees() {
    let s = Shape::Round { d: 0.1 };
    let pts = [v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(1.0, 0.0, 1.0)];
    let ps = route(&pts, &|_| 0.3, (0.0, 0.0)).unwrap();
    let e = &ps[1];
    let exact = s.area() * e.length();
    let m = sweep(s, &e.samples(64), 128);
    assert!(m.is_watertight());
    assert!(close(m.volume(), exact, exact * 2e-3));
    // vertical turn: the frame is carried along, side stays horizontal
    let Piece::Straight { frame, .. } = ps[2] else {
        panic!()
    };
    assert!(frame.side.z.abs() < 1e-12);
}

#[test]
fn route_errors() {
    let r = |_| 1.0;
    assert_eq!(
        route(&[v3(0.0, 0.0, 0.0)], &r, (0.0, 0.0)),
        Err(RouteError::TooFewPoints)
    );
    let pts = [v3(0.0, 0.0, 0.0), v3(0.5, 0.0, 0.0), v3(0.5, 0.5, 0.0)];
    assert!(matches!(
        route(&pts, &r, (0.0, 0.0)),
        Err(RouteError::TooShort { .. })
    ));
    let back = [v3(0.0, 0.0, 0.0), v3(5.0, 0.0, 0.0), v3(1.0, 0.0, 0.0)];
    assert_eq!(
        route(&back, &r, (0.0, 0.0)),
        Err(RouteError::Reversal { at: 1 })
    );
}

#[test]
fn transition_round_is_frustum() {
    let a = Section::new(Domain::Duct, Shape::Round { d: 0.4 });
    let b = Section::new(Domain::Duct, Shape::Round { d: 0.2 });
    let l = transition_length(&a, &b);
    assert!(close(l, 0.1 / (15f64).to_radians().tan(), 1e-12));
    let q = transition_qty(&a, &b, l);
    let exact = PI * l / 12.0 * (0.16 + 0.08 + 0.04);
    assert!(close(q.volume, exact, 1e-12));
}

#[test]
fn convex_clip_volume_exact() {
    let sq = |x0: f64, y0: f64, s: f64| {
        vec![
            v2(x0, y0),
            v2(x0 + s, y0),
            v2(x0 + s, y0 + s),
            v2(x0, y0 + s),
        ]
    };
    let a = Convex::prism(&sq(0.0, 0.0, 1.0), 0.0, 1.0);
    assert!(close(a.volume(), 1.0, 1e-12));
    let b = Convex::prism(&sq(0.5, 0.5, 1.0), 0.25, 2.0);
    assert!(close(a.intersection_volume(&b), 0.25 * 0.75, 1e-12));
    let far = Convex::prism(&sq(5.0, 5.0, 1.0), 0.0, 1.0);
    assert_eq!(a.intersection_volume(&far), 0.0);
    // skewed run through a wall
    let pipe = Convex::segment(
        &Shape::Round { d: 0.1 }.outline_equal_area(32),
        v3(-1.0, 0.5, 0.5),
        v3(2.0, 0.5, 0.5),
        v3(0.0, 1.0, 0.0),
        v3(0.0, 0.0, 1.0),
    );
    assert!(close(pipe.intersection_volume(&a), PI * 0.0025, 1e-12));
    // the opening is subtracted from the body
    let slab = Body::prism_with_holes(&sq(0.0, 0.0, 1.0), &[sq(0.25, 0.25, 0.5)], 0.0, 1.0);
    let probe = Body::solid(Convex::prism(&sq(-1.0, -1.0, 3.0), 0.0, 0.5));
    assert!(close(probe.intersection_volume(&slab), 0.5 * 0.75, 1e-12));
}

#[test]
fn clash_count_on_known_scene() {
    // 3 walls at x = 0, 4, 8; 2 pipes along x at y = 1 (crosses all walls) and y = 20 (none);
    // a vertical riser at x = 4 (passes through a wall)
    let walls: Vec<Body> = [0.0, 4.0, 8.0]
        .iter()
        .map(|&x| {
            Body::solid(Convex::prism(
                &clash::wall_footprint(v2(x, -5.0), v2(x, 5.0), 0.2, 0.0),
                0.0,
                3.0,
            ))
        })
        .collect();
    let seg = |a: V3, b: V3| {
        let f = start_frame((b - a).normalized());
        Body::solid(Convex::segment(
            &Shape::Round { d: 0.1 }.outline_equal_area(32),
            a,
            b,
            f.side,
            f.up,
        ))
    };
    let pipes = vec![
        seg(v3(-1.0, 1.0, 1.0), v3(9.0, 1.0, 1.0)),
        seg(v3(-1.0, 20.0, 1.0), v3(9.0, 20.0, 1.0)),
        seg(v3(4.0, 2.0, -1.0), v3(4.0, 2.0, 4.0)),
    ];
    let hits = clashes(&pipes, &walls, 1e-9);
    assert_eq!(hits.len(), 4);
    let a = PI * 0.0025;
    for h in &hits[..3] {
        assert_eq!(h.a, 0);
        assert!(close(h.volume, a * 0.2, 1e-12));
    }
    assert_eq!((hits[3].a, hits[3].b), (2, 1));
    assert!(close(hits[3].volume, a * 3.0, 1e-12));
}

#[test]
fn clash_10k_by_10k_is_fast() {
    let mut walls = vec![];
    let mut pipes = vec![];
    for i in 0..100 {
        for j in 0..100 {
            let (x, y) = (i as f64 * 3.0, j as f64 * 3.0);
            walls.push(Body::solid(Convex::prism(
                &clash::wall_footprint(v2(x, y), v2(x + 2.0, y), 0.2, 0.0),
                0.0,
                3.0,
            )));
            // every second pipe cuts the wall
            let yy = if (i + j) % 2 == 0 { y } else { y + 1.5 };
            let (a, b) = (v3(x + 1.0, yy - 0.5, 2.5), v3(x + 1.0, yy + 0.5, 2.5));
            let f = start_frame((b - a).normalized());
            pipes.push(Body::solid(Convex::segment(
                &Shape::Round { d: 0.05 }.outline_equal_area(16),
                a,
                b,
                f.side,
                f.up,
            )));
        }
    }
    let t = std::time::Instant::now();
    let hits = clashes(&pipes, &walls, 1e-12);
    let dt = t.elapsed().as_secs_f64();
    assert_eq!(hits.len(), 5000);
    let limit = if cfg!(debug_assertions) { 20.0 } else { 1.0 };
    assert!(dt < limit, "{dt} s");
}

#[test]
fn plan_single_double_and_rise() {
    let s = Shape::Rect { w: 0.4, h: 0.2 };
    let pts = [v3(0.0, 0.0, 3.0), v3(5.0, 0.0, 3.0), v3(5.0, 0.0, 6.0)];
    let ps = route(&pts, &|_| 0.2, (0.0, 0.0)).unwrap();
    let single = plan(s, &ps[0], Detail::Single, 16);
    assert_eq!(single.len(), 1);
    let double = plan(s, &ps[0], Detail::Double, 16);
    let PlanPrim::Edge(e) = &double[1] else {
        panic!()
    };
    assert!(close(e[0].y.abs(), 0.2, 1e-12));
    let rise = plan(s, &ps[2], Detail::Double, 16);
    assert!(matches!(rise[0], PlanPrim::Rise { .. }));
}

#[test]
fn section_cut_of_horizontal_duct() {
    let s = Shape::Rect { w: 0.4, h: 0.2 };
    let ps = route(
        &[v3(0.0, 0.0, 1.0), v3(4.0, 0.0, 1.0)],
        &|_| 0.2,
        (0.0, 0.0),
    )
    .unwrap();
    let cut = section_cut(
        s,
        &ps[0],
        Plane {
            n: v3(1.0, 0.0, 0.0),
            c: 2.0,
        },
        16,
    )
    .unwrap();
    assert_eq!(cut.len(), 4);
    assert!(cut.iter().all(|p| close(p.x, 2.0, 1e-12)));
    let a = crate::poly::area(&cut.iter().map(|p| v2(p.y, p.z)).collect::<Vec<_>>());
    assert!(close(a.abs(), 0.08, 1e-12));
}

#[test]
fn loft_round_to_rect_is_closed() {
    let f = start_frame(v3(1.0, 0.0, 0.0));
    let m = loft(
        Shape::Round { d: 0.2 },
        Shape::Rect { w: 0.4, h: 0.2 },
        v3(0.0, 0.0, 0.0),
        v3(0.5, 0.0, 0.0),
        f,
        32,
    );
    assert!(m.is_watertight() && m.volume() > 0.0);
    let p = radial(Shape::Oval { w: 0.5, h: 0.2 }, 0.0);
    assert!(close(p.x, 0.25, 1e-12));
    let p = radial(Shape::Oval { w: 0.5, h: 0.2 }, PI / 2.0);
    assert!(close(p.y, 0.1, 1e-12));
}

#[test]
fn zero_length_wall_footprint_is_empty_not_nan() {
    assert!(clash::wall_footprint(v2(1.0, 1.0), v2(1.0, 1.0), 0.2, 0.0).is_empty());
    let ok = clash::wall_footprint(v2(0.0, 0.0), v2(2.0, 0.0), 0.2, 0.0);
    assert_eq!(ok.len(), 4);
    assert!(ok.iter().all(|p| p.x.is_finite() && p.y.is_finite()));
}

#[test]
fn degenerate_direction_gives_a_finite_frame() {
    for t in [v3(0.0, 0.0, 0.0), v3(f64::NAN, 0.0, 1.0)] {
        let f = start_frame(t);
        assert!([f.side, f.up].iter().all(|v| v.x.is_finite() && v.y.is_finite() && v.z.is_finite()));
    }
    // vertical riser with no horizontal run
    let ps = route(&[v3(1.0, 1.0, 0.0), v3(1.0, 1.0, 0.0), v3(1.0, 1.0, 3.0)], &|_| 0.1, (0.0, 0.0)).unwrap();
    assert_eq!(ps.len(), 1);
    let m = sweep(Shape::Rect { w: 0.3, h: 0.1 }, &ps[0].samples(8), 24);
    assert!(m.positions.iter().all(|p| p.x.is_finite() && p.y.is_finite() && p.z.is_finite()));
    assert!(sweep(Shape::Round { d: 0.1 }, &[], 8).positions.is_empty());
}
