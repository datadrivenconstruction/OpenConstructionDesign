use std::f64::consts::{FRAC_PI_2, PI, TAU};

use super::*;

fn hosts() -> Vec<Host> {
    let w = Axes::world();
    let tilted = Axes::new(v3(1.0, -2.0, 0.5), v3(0.3, -0.2, 1.0), v3(1.0, 0.4, 0.0)).unwrap();
    vec![
        Host::Plane { ax: tilted, w: 12.0, h: 8.0 },
        Host::Cylinder {
            ax: Axes::new(v3(0.0, 0.0, 0.0), v3(0.0, 1.0, 0.0), v3(1.0, 0.0, 0.0)).unwrap(),
            r: 6.0,
            a0: 0.0,
            a1: PI,
            len: 20.0,
        },
        Host::Sphere {
            ax: w,
            r: 15.0,
            lon0: 0.0,
            lon1: TAU,
            lat0: 0.1,
            lat1: 1.2,
        },
        Host::Hypar {
            ax: tilted,
            lx: 16.0,
            ly: 12.0,
            rise: 3.0,
        },
        Host::Torus {
            ax: w,
            big: 20.0,
            r: 6.0,
            u0: 0.0,
            u1: FRAC_PI_2,
            v0: 0.0,
            v1: PI,
        },
        Host::Paraboloid { ax: tilted, f: 4.5, r0: 0.9, r1: 9.0 },
    ]
}

fn samples(h: &Host) -> Vec<(f64, f64)> {
    let ((u0, u1), (v0, v1)) = h.domain();
    let mut out = vec![];
    for i in 0..5 {
        for j in 0..5 {
            out.push((
                u0 + (u1 - u0) * (0.05 + 0.9 * i as f64 / 4.0),
                v0 + (v1 - v0) * (0.05 + 0.9 * j as f64 / 4.0),
            ));
        }
    }
    out
}

#[test]
fn derivatives_match_central_differences() {
    for h in hosts() {
        h.check().unwrap();
        for (u, v) in samples(&h) {
            let e = 1e-6;
            let fu = (h.point(u + e, v) - h.point(u - e, v)) * (0.5 / e);
            let fv = (h.point(u, v + e) - h.point(u, v - e)) * (0.5 / e);
            let (du, dv) = h.derivs(u, v);
            assert!((fu - du).len() < 1e-6 * du.len().max(1.0), "{} du at {u},{v}", h.kind());
            assert!((fv - dv).len() < 1e-6 * dv.len().max(1.0), "{} dv at {u},{v}", h.kind());
            let n = h.normal(u, v);
            assert!((n.len() - 1.0).abs() < 1e-12);
            assert!(n.dot(du).abs() < 1e-9 * du.len() && n.dot(dv).abs() < 1e-9 * dv.len());
        }
    }
}

#[test]
fn quadric_normals_point_outward() {
    let hs = hosts();
    // sphere: normal is the radius direction
    let s = &hs[2];
    for (u, v) in samples(s) {
        let p = s.point(u, v);
        assert!((p.len() - 15.0).abs() < 1e-9);
        assert!((s.normal(u, v) - p * (1.0 / 15.0)).len() < 1e-12);
    }
    // torus: normal is the direction from the tube centre circle
    let t = &hs[4];
    for (u, v) in samples(t) {
        let p = t.point(u, v);
        let c = v3(20.0 * u.cos(), 20.0 * u.sin(), 0.0);
        assert!(((p - c).len() - 6.0).abs() < 1e-9);
        assert!((t.normal(u, v) - (p - c) * (1.0 / 6.0)).len() < 1e-12);
    }
}

#[test]
fn projection_recovers_parameters() {
    for h in hosts() {
        for (u, v) in samples(&h) {
            let p = h.point(u, v);
            let (pu, pv) = h.project(p);
            assert!((h.point(pu, pv) - p).len() < 1e-9, "{} at {u},{v}", h.kind());
            // off the surface along the normal: same foot point
            let q = p + h.normal(u, v) * 0.3;
            let (qu, qv) = h.project(q);
            assert!((h.point(qu, qv) - p).len() < 1e-7, "{} offset at {u},{v}", h.kind());
        }
    }
}

#[test]
fn areas_match_closed_forms() {
    let hs = hosts();
    let close = |a: f64, b: f64| (a - b).abs() < 1e-10 * b;
    assert!(close(hs[0].area(), 96.0));
    assert!(close(hs[1].area(), 6.0 * PI * 20.0));
    // spherical zone: 2 pi r^2 (sin lat1 - sin lat0)
    assert!(close(hs[2].area(), TAU * 225.0 * (1.2f64.sin() - 0.1f64.sin())));
    // torus patch: r du (R dv + r (sin v1 - sin v0))
    assert!(close(hs[4].area(), 6.0 * FRAC_PI_2 * (20.0 * PI)));
    // hypar: larger than its plan, by the slope
    assert!(hs[3].area() > 16.0 * 12.0);
    // paraboloid zone z = a rho^2: pi / (6 a^2) [(1 + 4 a^2 rho^2)^1.5] between the rings
    let a: f64 = 1.0 / 18.0;
    let ring = |r: f64| (1.0 + 4.0 * a * a * r * r).powf(1.5);
    assert!(close(hs[5].area(), PI / (6.0 * a * a) * (ring(9.0) - ring(0.9))));
}

/// A 18 m dish 4.5 m deep: the rim lies on the asked circle and depth, every point is as far
/// from the focus as from the directrix, and the axis follows the tilt and azimuth.
#[test]
fn a_dish_paraboloid_has_its_rim_focus_and_tilt() {
    let flat = Host::paraboloid(v3(2.0, 3.0, 1.0), 9.0, 4.5, 0.0, 0.0, 0.0).unwrap();
    let Host::Paraboloid { f, r0, r1, .. } = flat else { panic!("{}", flat.kind()) };
    assert!((f - 4.5).abs() < 1e-12 && (r0 - 0.9).abs() < 1e-12 && r1 == 9.0);
    assert!(flat.closed_u());
    let focus = v3(2.0, 3.0, 1.0 + f);
    for (u, v) in samples(&flat) {
        let p = flat.point(u, v);
        assert!(((p - focus).len() - (p.z - (1.0 - f))).abs() < 1e-9, "focus at {u},{v}");
        assert!(flat.normal(u, v).z < 0.0, "normal away from the focus");
    }
    for k in 0..8 {
        let p = flat.point(k as f64 * TAU / 8.0, 9.0);
        assert!((v3(p.x - 2.0, p.y - 3.0, 0.0).len() - 9.0).abs() < 1e-9 && (p.z - 5.5).abs() < 1e-9);
    }
    let tilt = 0.6;
    let up = Host::paraboloid(v3(0.0, 0.0, 0.0), 9.0, 4.5, 1.2, tilt, FRAC_PI_2).unwrap();
    let z = up.axes().z;
    assert!((z - v3(0.0, tilt.sin(), tilt.cos())).len() < 1e-12);
    // the rim centre sits on the tilted axis, one depth from the vertex
    let rim: V3 = (0..8).map(|k| up.point(k as f64 * TAU / 8.0, 9.0)).fold(v3(0.0, 0.0, 0.0), |s, p| s + p);
    assert!((rim * (1.0 / 8.0) - z * 4.5).len() < 1e-9);
    assert!(Host::paraboloid(v3(0.0, 0.0, 0.0), 9.0, 10.0, 0.0, 0.0, 0.0).is_err());
    assert!(Host::paraboloid(v3(0.0, 0.0, 0.0), 9.0, 4.5, 9.5, 0.0, 0.0).is_err());
    assert!(Host::paraboloid(v3(0.0, 0.0, 0.0), 9.0, 4.5, 0.0, FRAC_PI_2, 0.0).is_err());
}

#[test]
fn text_round_trips_and_bad_hosts_are_refused() {
    for h in hosts() {
        let back = Host::parse(&h.text()).unwrap();
        assert_eq!(back.kind(), h.kind());
        for (u, v) in samples(&h) {
            assert!((back.point(u, v) - h.point(u, v)).len() < 1e-12);
        }
    }
    assert!(Host::parse("cone 0 0 0 0 0 1 1 0 0 1 2").is_none());
    let w = Axes::world();
    let pole = Host::Sphere {
        ax: w,
        r: 5.0,
        lon0: 0.0,
        lon1: TAU,
        lat0: 0.0,
        lat1: FRAC_PI_2,
    };
    assert!(pole.check().unwrap_err().contains("poles"));
    let fat = Host::Torus {
        ax: w,
        big: 2.0,
        r: 3.0,
        u0: 0.0,
        u1: 1.0,
        v0: 0.0,
        v1: 1.0,
    };
    assert!(fat.check().is_err());
    assert!(Axes::new(v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), v3(0.0, 0.0, 2.0)).is_none());
}
