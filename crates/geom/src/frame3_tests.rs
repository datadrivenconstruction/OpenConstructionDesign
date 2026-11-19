use super::*;
use crate::arc::BulgeRing;
use crate::csg::Body;
use crate::gsolid::{self, Form, Frame, Path3, Section};
use crate::math::{v2, Tol};

fn circle(r: f64) -> Section {
    Section {
        outer: BulgeRing {
            pts: vec![v2(r, 0.0), v2(-r, 0.0)],
            bulges: vec![1.0, 1.0],
        },
        holes: vec![],
    }
}

fn rect(w: f64, h: f64) -> Section {
    Section::polygon(vec![
        v2(-w / 2.0, -h / 2.0),
        v2(w / 2.0, -h / 2.0),
        v2(w / 2.0, h / 2.0),
        v2(-w / 2.0, h / 2.0),
    ])
}

fn helix(r: f64, turns: f64, pitch: f64) -> Smooth {
    Smooth::new(
        v3(r, 0.0, 0.0),
        vec![Seg::Helix {
            center: v3(0.0, 0.0, 0.0),
            axis: v3(0.0, 0.0, 1.0),
            angle: TAU * turns,
            rise: pitch * turns,
        }],
    )
}

fn sweep(profile: Section, sm: Smooth) -> Form {
    Form::Sweep {
        profile,
        path: Path3::smooth(sm),
    }
}

fn rel(a: f64, b: f64) -> f64 {
    (a - b).abs() / b.abs().max(1e-300)
}

#[test]
fn arc_through_three_points_in_a_tilted_plane() {
    let (a, m, b) = (v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 1.0), v3(-1.0, 0.0, 2.0));
    let sm = Smooth::new(a, vec![Seg::Arc { via: m, to: b }]);
    let g = sm.geos().unwrap();
    let Geo::Circ { c, r, theta, .. } = g[0] else {
        panic!("arc expected");
    };
    for p in [a, m, b] {
        assert!(((p - c).len() - r).abs() < 1e-12);
    }
    assert!((g[0].at(1.0)[0] - b).len() < 1e-12);
    assert!(rel(sm.length().unwrap(), r * theta) < 1e-14);
    // the arc passes the via point
    let near = (0..=1000)
        .map(|k| (g[0].at(k as f64 / 1000.0)[0] - m).len())
        .fold(f64::MAX, f64::min);
    assert!(near < 1e-2 * r);
}

#[test]
fn nurbs_length_by_gauss_matches_exact_arc() {
    let cu = Curve::arc(
        v3(0.0, 0.0, 0.0),
        v3(1.0, 0.0, 0.0),
        v3(0.0, 1.0, 0.0),
        2.0,
        0.0,
        2.5,
    )
    .unwrap();
    let start = cu.point(cu.domain().0);
    let sm = Smooth::new(start, vec![Seg::Nurbs(cu)]);
    assert!(rel(sm.length().unwrap(), 5.0) < 1e-12);
}

#[test]
fn helix_length_and_volume_are_exact() {
    let sm = helix(1.0, 3.0, 0.5);
    let l = 3.0 * (TAU * TAU + 0.25f64).sqrt();
    assert!(rel(sm.length().unwrap(), l) < 1e-14);
    let r = 0.03;
    let q = gsolid::quantities(&sweep(circle(r), sm.clone())).unwrap();
    let a = PI * r * r;
    assert!(rel(q.volume, a * l) < 1e-9, "{} vs {}", q.volume, a * l);
    // lateral area of a tube: 2πr·L (curvature terms cancel over the circle) + two caps
    assert!(rel(q.area, TAU * r * l + 2.0 * a) < 1e-5, "{}", q.area);
    // the mesh converges to it and is a closed solid
    let m = gsolid::mesh(&sweep(circle(r), sm), &Frame::default(), 1e-4).unwrap();
    assert!(rel(m.volume().abs(), a * l) < 6e-3, "{}", m.volume());
    assert!(Body::from_mesh(&m, 0).check_closed(Tol::default()).is_ok());
}

#[test]
fn rmf_closes_on_a_planar_circle_without_drift() {
    // full circle of radius 2 in a tilted plane, two arcs
    let n = v3(0.0, -0.6, 0.8);
    let x = v3(1.0, 0.0, 0.0);
    let y = n.cross(x);
    let p = |a: f64| x * (2.0 * a.cos()) + y * (2.0 * a.sin());
    let sm = Smooth::new(
        p(0.0),
        vec![
            Seg::Arc {
                via: p(PI / 2.0),
                to: p(PI),
            },
            Seg::Arc {
                via: p(1.5 * PI),
                to: p(0.0),
            },
        ],
    );
    let sts = stations(&sm, 1e-4, 0.05).unwrap();
    assert!(sts.closed);
    // a planar RMF keeps a fixed angle to the plane normal all the way round
    let first = sts.st[0].u.dot(n);
    for s in &sts.st {
        assert!((s.u.dot(n) - first).abs() < 1e-9, "{} vs {first}", s.u.dot(n));
    }
    // and the profile ring at the seam meets the first one
    let q = v2(0.1, 0.05);
    let last = sts.st.last().unwrap();
    let back = reflect(last.p, last.t, last.u, sts.st[0].p, sts.st[0].t);
    assert!((back - sts.st[0].u).len() < 1e-6);
    assert!((sts.st[0].place(q) - p(0.0)).len() < 0.2);
}

#[test]
fn rmf_on_a_helix_is_continuous_and_spins_by_torsion() {
    let (r, pitch, turns) = (1.5, 0.8, 2.0);
    let sm = helix(r, turns, pitch);
    let sts = stations(&sm, 1e-5, 0.01).unwrap();
    let st = &sts.st;
    // continuity: neighbours differ by little more than the tangent turn
    for w in st.windows(2) {
        let moved = transport(w[0].u, w[0].t, w[1].t);
        assert!((moved - w[1].u).len() < 1e-4, "{}", (moved - w[1].u).len());
        assert!((w[1].u.dot(w[1].t)).abs() < 1e-12);
    }
    // relative to the Frenet frame the RMF turns by -τ·L
    let c = pitch / TAU;
    let tau = c / (r * r + c * c);
    let ang = |s: &Station| signed_angle(s.nrm, s.u, s.t);
    let mut total = 0.0;
    for w in st.windows(2) {
        let mut d = ang(&w[1]) - ang(&w[0]);
        if d > PI {
            d -= TAU;
        } else if d < -PI {
            d += TAU;
        }
        total += d;
    }
    let want = -tau * sts.length;
    assert!((total - want).abs() < 1e-6 * want.abs(), "{total} vs {want}");
}

#[test]
fn fixed_frame_keeps_a_handrail_upright() {
    let mut sm = helix(1.2, 1.25, 3.0);
    sm.frame = FrameMode::Fixed(v3(0.0, 0.0, 1.0));
    let sts = stations(&sm, 1e-4, 0.02).unwrap();
    for s in &sts.st {
        // u horizontal: the profile "up" axis lies in the vertical plane of the tangent
        assert!(s.u.z.abs() < 1e-12);
        assert!(s.v.z > 0.0);
    }
}

#[test]
fn helix_sweep_has_no_self_intersection() {
    let (r, rho) = (0.6, 0.03);
    let sm = helix(r, 2.0, 0.25);
    let f = sweep(circle(rho), sm.clone());
    gsolid::validate(&f).unwrap();
    let m = gsolid::mesh(&f, &Frame::default(), 1e-4).unwrap();
    // every surface point is nearest to its own part of the axis: no coil reaches another
    let dense = stations(&sm, 1e-6, 0.002).unwrap();
    let axis: Vec<V3> = dense.st.iter().map(|s| s.p).collect();
    for p in m.positions.iter().step_by(13) {
        let d = axis
            .windows(2)
            .map(|w| seg_dist_pt(*p, w[0], w[1]))
            .fold(f64::MAX, f64::min);
        assert!((d - rho).abs() < 2e-4, "{d}");
    }
    // tight pitch: the coils touch, refused with the place named
    let e = gsolid::validate(&sweep(circle(rho), helix(r, 2.0, 0.05))).unwrap_err();
    assert!(e.to_string().contains("touches itself"), "{e}");
    // radius below the profile: refused at a station
    let e = gsolid::validate(&sweep(circle(0.2), helix(0.1, 0.5, 0.2))).unwrap_err();
    assert!(e.to_string().contains("curvature radius"), "{e}");
    assert!(e.to_string().contains("station"), "{e}");
}

#[test]
fn twist_along_an_s_curve() {
    let mut sm = Smooth::new(
        v3(0.0, 0.0, 0.0),
        vec![Seg::Spline {
            via: vec![v3(2.0, 1.5, 0.3), v3(4.0, -1.5, 0.6)],
            to: v3(6.0, 0.0, 1.0),
        }],
    );
    sm.twist = Law::Linear(0.0, PI / 2.0);
    let f = sweep(rect(0.3, 0.1), sm.clone());
    gsolid::validate(&f).unwrap();
    let q = gsolid::quantities(&f).unwrap();
    let l = sm.length().unwrap();
    assert!(rel(q.volume, 0.03 * l) < 1e-12);
    let m = gsolid::mesh(&f, &Frame::default(), 1e-4).unwrap();
    assert!(Body::from_mesh(&m, 0).check_closed(Tol::default()).is_ok());
    assert!(rel(m.volume().abs(), 0.03 * l) < 1e-3, "{}", m.volume());
    // the end section is turned a quarter relative to the untwisted frame
    let sts = stations(&sm, 1e-4, 0.02).unwrap();
    sm.twist = Law::constant(0.0);
    let flat = stations(&sm, 1e-4, 0.02).unwrap();
    let (a, b) = (sts.st.last().unwrap(), flat.st.last().unwrap());
    assert!((signed_angle(b.u, a.u, a.t) - PI / 2.0).abs() < 1e-9);
}

#[test]
fn scale_law_volume_is_exact() {
    let mut sm = Smooth::new(v3(0.0, 0.0, 0.0), vec![Seg::Line { to: v3(4.0, 0.0, 0.0) }]);
    sm.scale = Law::Ease(1.0, 0.5);
    let q = gsolid::quantities(&sweep(rect(0.2, 0.2), sm.clone())).unwrap();
    let want = 0.04 * 4.0 * gauss(0.0, 1.0, |t| sm.scale.at(t).powi(2));
    assert!(rel(q.volume, want) < 1e-12);
    // Ease(1, 0.5)²: ∫ = 1 - 2·0.5·(1/2) + 0.25·(13/35)
    let exact = 0.04 * 4.0 * (1.0 - 0.5 + 0.25 * 13.0 / 35.0);
    assert!(rel(q.volume, exact) < 1e-12, "{} vs {exact}", q.volume);
}

#[test]
fn offset_profile_on_an_arc_follows_pappus() {
    // horizontal quarter arc R = 3, square 0.2 offset 0.5 towards the centre: centroid R' = 2.5
    let sm = Smooth::from_polyline(&[v3(3.0, 0.0, 0.0), v3(0.0, 3.0, 0.0)], &[(PI / 8.0).tan()]);
    let prof = Section::polygon(vec![
        v2(0.4, -0.1),
        v2(0.6, -0.1),
        v2(0.6, 0.1),
        v2(0.4, 0.1),
    ]);
    let q = gsolid::quantities(&sweep(prof, sm)).unwrap();
    let want = 0.04 * 2.5 * PI / 2.0;
    assert!(rel(q.volume, want) < 1e-6, "{} vs {want}", q.volume);
}

#[test]
fn kinks_are_mitred_and_curves_are_not() {
    let sm = Smooth::new(
        v3(0.0, 0.0, 0.0),
        vec![
            Seg::Line { to: v3(2.0, 0.0, 0.0) },
            Seg::Arc {
                via: v3(2.0 + 2f64.sqrt(), 2.0 - 2f64.sqrt(), 0.0),
                to: v3(4.0, 2.0, 0.0),
            },
            Seg::Line { to: v3(4.0, 4.0, 1.0) },
        ],
    );
    let sts = stations(&sm, 1e-3, 0.05).unwrap();
    let kinks: Vec<_> = sts.st.iter().filter(|s| s.kink).collect();
    // line → tangent arc is smooth; arc end → sloped line is a kink
    assert_eq!(kinks.len(), 1);
    assert!((kinks[0].p - v3(4.0, 2.0, 0.0)).len() < 1e-12);
    let f = sweep(rect(0.2, 0.2), sm.clone());
    let q = gsolid::quantities(&f).unwrap();
    assert!(rel(q.volume, 0.04 * sm.length().unwrap()) < 1e-12);
}

#[test]
fn closed_loop_refuses_twist() {
    let p = |a: f64| v3(2.0 * a.cos(), 2.0 * a.sin(), 0.0);
    let mut sm = Smooth::new(
        p(0.0),
        vec![
            Seg::Arc {
                via: p(PI / 2.0),
                to: p(PI),
            },
            Seg::Arc {
                via: p(1.5 * PI),
                to: p(0.0),
            },
        ],
    );
    let q = gsolid::quantities(&sweep(circle(0.1), sm.clone())).unwrap();
    assert!(rel(q.volume, PI * 0.01 * 4.0 * PI) < 1e-12);
    sm.twist = Law::Linear(0.0, 1.0);
    assert!(stations(&sm, 1e-3, 0.05).is_err());
}

#[test]
fn polyline_bulges_become_exact_arcs() {
    let b = (PI / 8.0).tan();
    let sm = Smooth::from_polyline(&[v3(3.0, 0.0, 1.0), v3(0.0, 3.0, 2.0)], &[b]);
    let v = sm.vertices();
    assert!((v[1] - v3(0.0, 3.0, 2.0)).len() < 1e-12);
    let l = (3.0 * PI / 2.0).hypot(1.0);
    assert!(rel(sm.length().unwrap(), l) < 1e-14);
}

#[test]
fn grips_edit_the_helix_and_the_spline() {
    let sm = Smooth::new(
        v3(1.0, 0.0, 0.0),
        vec![
            Seg::Helix {
                center: v3(0.0, 0.0, 0.0),
                axis: v3(0.0, 0.0, 1.0),
                angle: 1.5 * PI,
                rise: 1.0,
            },
            Seg::Spline {
                via: vec![v3(1.0, -1.0, 1.5)],
                to: v3(2.0, -1.0, 2.0),
            },
        ],
    );
    let gs = grips(&sm);
    for g in &gs {
        assert_eq!(GripKey::from_code(g.key.code()), Some(g.key));
    }
    // the helix end is derived: no vertex grip for it
    assert!(!gs.iter().any(|g| g.key == GripKey::Vertex(1)));
    let rise = gs.iter().find(|g| g.key == GripKey::Rise(0)).unwrap();
    let up = grip_apply(&sm, rise.key, rise.pos + v3(0.0, 0.0, 0.5)).unwrap();
    assert!(matches!(up.segs[0], Seg::Helix { rise, .. } if (rise - 1.5).abs() < 1e-12));
    let turn = gs.iter().find(|g| g.key == GripKey::Turn(0)).unwrap();
    let full = grip_apply(&sm, turn.key, v3(0.0, 1.0, 5.0)).unwrap();
    assert!(matches!(full.segs[0], Seg::Helix { angle, .. } if (angle - PI / 2.0).abs() < 1e-12));
    let via = gs.iter().find(|g| g.key == GripKey::Via(1, 0)).unwrap();
    let moved = grip_apply(&sm, via.key, v3(1.5, -1.5, 1.5)).unwrap();
    assert!(matches!(&moved.segs[1], Seg::Spline { via, .. } if via[0] == v3(1.5, -1.5, 1.5)));
    let end = gs.iter().find(|g| g.key == GripKey::Vertex(2)).unwrap();
    let e = grip_apply(&sm, end.key, v3(3.0, -1.0, 2.0)).unwrap();
    assert!((e.vertices()[2] - v3(3.0, -1.0, 2.0)).len() < 1e-12);
}
