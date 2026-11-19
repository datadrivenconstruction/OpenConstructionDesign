use super::*;
use crate::csg::BoolOp;
use crate::math::{v3, Tol, V3};
use crate::mesh::Mesh;
use std::collections::BTreeMap;
use std::f64::consts::{FRAC_PI_2, PI, TAU};

fn dist(a: V3, b: V3) -> f64 {
    (a - b).len()
}

const O: V3 = v3(0.0, 0.0, 0.0);
const X: V3 = v3(1.0, 0.0, 0.0);
const Y: V3 = v3(0.0, 1.0, 0.0);
const Z: V3 = v3(0.0, 0.0, 1.0);

fn rational_cubic() -> Curve {
    Curve::new(
        3,
        vec![0., 0., 0., 0., 0.3, 0.7, 1., 1., 1., 1.],
        vec![
            v3(0., 0., 0.),
            v3(1., 2., 0.5),
            v3(2., -1., 1.),
            v3(3., 3., 0.),
            v3(4., 0.5, -1.),
            v3(5., 1., 0.),
        ],
        vec![1., 0.5, 2., 1., 0.8, 1.3],
    )
    .unwrap()
}

/// Undirected edge -> (uses, +1 per a<b direction, -1 per the other).
fn edge_uses(m: &Mesh) -> BTreeMap<(u32, u32), (u32, i32)> {
    let mut e = BTreeMap::new();
    for t in &m.triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let s = e.entry((a.min(b), a.max(b))).or_insert((0u32, 0i32));
            s.0 += 1;
            s.1 += if a < b { 1 } else { -1 };
        }
    }
    e
}

fn assert_closed(m: &Mesh) {
    assert!(!m.triangles.is_empty());
    for (e, (n, bal)) in edge_uses(m) {
        assert!(n == 2 && bal == 0, "edge {e:?}: {n} uses, balance {bal}");
    }
    assert!(m.is_watertight());
}

/// Dense barycentric samples of every triangle stay within `chord` of the surface.
fn assert_within(s: &Surface, sm: &SurfaceMesh, chord: f64) {
    let m = &sm.mesh;
    for t in &m.triangles {
        let [a, b, c] = t.map(|i| i as usize);
        for i in 0..=6 {
            for j in 0..=6 - i {
                let l = [i as f64 / 6.0, j as f64 / 6.0, (6 - i - j) as f64 / 6.0];
                let u = l[0] * sm.uv[a][0] + l[1] * sm.uv[b][0] + l[2] * sm.uv[c][0];
                let v = l[0] * sm.uv[a][1] + l[1] * sm.uv[b][1] + l[2] * sm.uv[c][1];
                let lin = m.positions[a] * l[0] + m.positions[b] * l[1] + m.positions[c] * l[2];
                let d = dist(s.point(u, v), lin);
                assert!(d <= chord, "deviation {d} > {chord}");
            }
        }
    }
}

#[test]
fn circle_nurbs_exact_radius() {
    let (c, xa, ya, r) = (v3(1., -2., 0.5), v3(1., 1., 0.), v3(-1., 1., 1.), 2.5);
    let circ = Curve::circle(c, xa, ya, r).unwrap();
    assert_eq!(circ.degree(), 2);
    assert_eq!(circ.ctrl().len(), 9);
    let n = xa.cross(ya).normalized();
    for k in 0..=1000 {
        let p = circ.point(k as f64 / 1000.0);
        assert!(((p - c).len() - r).abs() <= 1e-12);
        assert!((p - c).dot(n).abs() <= 1e-12);
    }
    assert!(dist(circ.point(0.0), circ.point(1.0)) <= 1e-12);
    let (x, y) = frame(xa, ya).unwrap();
    let arc = Curve::arc(c, xa, ya, r, 0.3, 2.0).unwrap();
    assert!(dist(arc.point(0.0), c + (x * 0.3f64.cos() + y * 0.3f64.sin()) * r) <= 1e-12);
    assert!(dist(arc.point(1.0), c + (x * 2.0f64.cos() + y * 2.0f64.sin()) * r) <= 1e-12);
    for k in 0..=100 {
        assert!(((arc.point(k as f64 / 100.0) - c).len() - r).abs() <= 1e-12);
    }
}

#[test]
fn derivatives_match_finite_difference() {
    let c = rational_cubic();
    let h = 1e-6;
    for u in [0.1, 0.45, 0.62, 0.9] {
        let d = c.derivs(u, 2);
        assert!(dist(d[0], c.point(u)) <= 1e-12);
        let fd1 = (c.point(u + h) - c.point(u - h)) * (0.5 / h);
        assert!(dist(d[1], fd1) <= 1e-5 * (1.0 + d[1].len()), "C' at {u}");
        let fd2 = (c.derivs(u + h, 1)[1] - c.derivs(u - h, 1)[1]) * (0.5 / h);
        assert!(dist(d[2], fd2) <= 1e-4 * (1.0 + d[2].len()), "C'' at {u}");
    }
    let s = revolve(&c, v3(-1., 0., 0.), Y, 1.2).unwrap();
    for (u, v) in [(0.2, 0.33), (0.55, 0.62), (0.8, 0.1)] {
        let d = s.derivs(u, v, 2);
        assert!(dist(d[0][0], s.point(u, v)) <= 1e-11);
        let fu = (s.point(u + h, v) - s.point(u - h, v)) * (0.5 / h);
        let fv = (s.point(u, v + h) - s.point(u, v - h)) * (0.5 / h);
        assert!(dist(d[1][0], fu) <= 1e-5 * (1.0 + fu.len()), "Su");
        assert!(dist(d[0][1], fv) <= 1e-5 * (1.0 + fv.len()), "Sv");
        let fuv = (s.derivs(u, v + h, 1)[1][0] - s.derivs(u, v - h, 1)[1][0]) * (0.5 / h);
        let fuu = (s.derivs(u + h, v, 1)[1][0] - s.derivs(u - h, v, 1)[1][0]) * (0.5 / h);
        let fvv = (s.derivs(u, v + h, 1)[0][1] - s.derivs(u, v - h, 1)[0][1]) * (0.5 / h);
        assert!(dist(d[1][1], fuv) <= 1e-4 * (1.0 + fuv.len()), "Suv");
        assert!(dist(d[2][0], fuu) <= 1e-4 * (1.0 + fuu.len()), "Suu");
        assert!(dist(d[0][2], fvv) <= 1e-4 * (1.0 + fvv.len()), "Svv");
        let n = s.normal(u, v).unwrap();
        assert!(n.cross(d[1][0].cross(d[0][1]).normalized()).len() <= 1e-12);
    }
}

#[test]
fn knot_insertion_preserves_shape() {
    let c = rational_cubic();
    let r = c
        .insert_knot(0.3, 2)
        .insert_knot(0.5, 3)
        .refine(&[0.123, 0.123, 0.9]);
    assert_eq!(r.ctrl().len(), 6 + 2 + 3 + 2 + 1);
    assert_eq!(r.knots().len(), r.ctrl().len() + 4);
    // Multiplicity is capped at the degree.
    assert_eq!(r.insert_knot(0.5, 1).ctrl().len(), r.ctrl().len());
    assert_eq!(c.insert_knot(0.0, 1).ctrl().len(), 6);
    assert_eq!(c.insert_knot(1.0, 1).ctrl().len(), 6);
    for k in 0..=200 {
        let u = k as f64 / 200.0;
        assert!(dist(r.point(u), c.point(u)) <= 1e-12, "at {u}");
    }
}

#[test]
fn degree_elevation_preserves_shape() {
    let c = rational_cubic();
    let e = c.elevated();
    assert_eq!(e.degree(), 4);
    assert_eq!(e.ctrl().len(), 4 * 3 + 1);
    for k in 0..=200 {
        let u = k as f64 / 200.0;
        assert!(dist(e.point(u), c.point(u)) <= 1e-12);
    }
    let circ = Curve::circle(O, X, Y, 1.0).unwrap().elevated().elevated();
    for k in 0..=200 {
        assert!((circ.point(k as f64 / 200.0).len() - 1.0).abs() <= 1e-12);
    }
}

#[test]
fn interpolation_passes_through_points() {
    let pts = [
        v3(0., 0., 0.),
        v3(1., 0.5, 0.),
        v3(2., 2., 0.3),
        v3(3., 1.5, 0.),
        v3(4.5, 0., -0.2),
        v3(5., 1., 0.),
    ];
    let c = Curve::interpolate(&pts, 3).unwrap();
    let t = chord_params(&[pts.to_vec()]).unwrap();
    for (k, p) in pts.iter().enumerate() {
        assert!(dist(c.point(t[k]), *p) <= 1e-12);
    }
    assert_eq!(Curve::interpolate(&pts[..2], 3).unwrap().degree(), 1);
}

#[test]
fn loft_surface_interpolates_curves() {
    let c0 = Curve::arc(O, X, Y, 1.0, 0.0, PI).unwrap();
    let c1 = Curve::interpolate(
        &[
            v3(1., 0., 1.),
            v3(0.8, 0.6, 1.),
            v3(0.2, 1.1, 1.),
            v3(-0.4, 0.9, 1.),
            v3(-0.9, 0.4, 1.),
            v3(-1.1, 0., 1.),
        ],
        3,
    )
    .unwrap();
    let c2 = Curve::polyline(&[v3(1.2, 0., 2.), v3(-1.2, 0., 2.)]).unwrap();
    let curves = [c0.clone(), c1, c2];
    let lf = loft(&curves, 2).unwrap();
    assert_eq!(lf.params.len(), 3);
    assert_eq!((lf.surface.deg_u(), lf.surface.deg_v()), (3, 2));
    for (k, c) in curves.iter().enumerate() {
        for i in 0..=50 {
            let u = i as f64 / 50.0;
            let d = dist(lf.surface.point(u, lf.params[k]), c.point(u));
            assert!(d <= 1e-9, "curve {k} at {u}: {d}");
        }
    }
    // Ruled: ends reproduce the curves, inner points lie on straight rulings.
    let rs = ruled(&c0, &curves[2]).unwrap();
    for i in 0..=20 {
        let u = i as f64 / 20.0;
        let (a, b) = (rs.point(u, 0.0), rs.point(u, 1.0));
        assert!(dist(a, c0.point(u)) <= 1e-12);
        assert!(dist(b, curves[2].point(u)) <= 1e-12);
        let m = rs.point(u, 0.37);
        assert!((m - a).cross(b - a).len() <= 1e-12);
    }
}

#[test]
fn tessellation_within_chord_tol() {
    let tol = TessTol::default();
    let circ = Curve::circle(O, X, Y, 1.0).unwrap();
    let ct = circ.tessellate(&tol);
    assert!(ct.converged);
    assert!(ct.max_dev <= tol.chord);
    assert!(ct.points.len() >= 33 && ct.points.len() <= 513, "{}", ct.points.len());
    for w in ct.params.windows(2) {
        let (pa, pb) = (circ.point(w[0]), circ.point(w[1]));
        for k in 1..16 {
            let t = k as f64 / 16.0;
            let q = circ.point(w[0] + (w[1] - w[0]) * t);
            assert!(dist(q, pa + (pb - pa) * t) <= tol.chord);
        }
    }
    // Twisted bilinear patch: only the diagonals deviate.
    let a = Curve::polyline(&[v3(0., 0., 0.), v3(2., 0., 0.)]).unwrap();
    let b = Curve::polyline(&[v3(0., 2., 1.), v3(2., 2., -1.)]).unwrap();
    let saddle = ruled(&a, &b).unwrap();
    // Open lofted half-tube with changing radius.
    let half = |r: f64, z: f64| Curve::arc(v3(0., 0., z), X, Y, r, 0.0, PI).unwrap();
    let tube = loft(&[half(1.0, 0.0), half(1.5, 1.0), half(0.8, 2.0)], 2)
        .unwrap()
        .surface;
    for s in [&saddle, &tube] {
        let sm = tessellate(s, &tol);
        assert!(sm.converged);
        assert!(sm.max_dev <= tol.chord);
        assert_within(s, &sm, tol.chord);
    }
}

#[test]
fn loft_tessellation_watertight() {
    let circ = |r: f64, z: f64| Curve::circle(v3(0., 0., z), X, Y, r).unwrap();
    let lf = loft(&[circ(1.0, 0.0), circ(1.5, 1.0), circ(1.0, 2.0)], 2).unwrap();
    assert!(lf.surface.closed_u());
    let tol = TessTol::default();
    let sm = tessellate(&lf.surface, &tol);
    assert!(sm.converged);
    let mut boundary = 0;
    for (&(a, b), &(n, bal)) in &edge_uses(&sm.mesh) {
        assert!(n <= 2, "edge used {n} times");
        if n == 2 {
            assert_eq!(bal, 0, "inconsistent orientation");
        } else {
            boundary += 1;
            for v in [a, b] {
                let z = sm.mesh.positions[v as usize].z;
                assert!(z.abs() < 1e-9 || (z - 2.0).abs() < 1e-9, "crack at z = {z}");
            }
        }
    }
    assert_eq!(boundary, 2 * (sm.us.len() - 1));
    let again = tessellate(&lf.surface, &tol);
    assert_eq!(again.mesh.positions, sm.mesh.positions);
    assert_eq!(again.mesh.triangles, sm.mesh.triangles);
}

#[test]
fn thicken_closed_volume() {
    let tol = TessTol::default();
    // Flat 2 x 3 patch.
    let a = Curve::polyline(&[v3(0., 0., 0.), v3(2., 0., 0.)]).unwrap();
    let b = Curve::polyline(&[v3(0., 3., 0.), v3(2., 3., 0.)]).unwrap();
    let th = thicken(&ruled(&a, &b).unwrap(), 0.1, &tol).unwrap();
    assert_closed(&th.mesh);
    assert!((th.mesh.volume() - 0.6).abs() < 1e-9);
    assert_eq!(th.losses.len(), 2);
    // Cylinder wall: closed seam, two rims.
    let c0 = Curve::circle(O, X, Y, 1.0).unwrap();
    let cyl = loft(&[c0.clone(), c0.translated(v3(0., 0., 2.))], 1)
        .unwrap()
        .surface;
    let th = thicken(&cyl, 0.1, &tol).unwrap();
    assert_closed(&th.mesh);
    let p = cyl.point(0.1, 0.5);
    let out = cyl.normal(0.1, 0.5).unwrap().dot(v3(p.x, p.y, 0.)) > 0.0;
    let exact = if out { PI * (1.21 - 1.0) * 2.0 } else { PI * (1.0 - 0.81) * 2.0 };
    assert!((th.mesh.volume() - exact).abs() < 2e-3 * exact);
    // Hemisphere: closed seam plus a pole.
    let prof = Curve::arc(O, Z, X, 1.0, 0.0, FRAC_PI_2).unwrap();
    let sph = revolve(&prof, O, Z, TAU).unwrap();
    assert!(sph.closed_u() && sph.pole_v(false) && !sph.pole_v(true));
    for i in 0..=10 {
        for j in 0..=10 {
            let q = sph.point(i as f64 / 10.0, j as f64 / 10.0);
            assert!((q.len() - 1.0).abs() <= 1e-12);
        }
    }
    let th = thicken(&sph, 0.1, &tol).unwrap();
    assert_closed(&th.mesh);
    let p = sph.point(0.3, 0.5);
    let out = sph.normal(0.3, 0.5).unwrap().dot(p) > 0.0;
    let exact = 2.0 / 3.0 * PI * if out { 1.331 - 1.0 } else { 1.0 - 0.729 };
    assert!((th.mesh.volume() - exact).abs() < 1e-2 * exact);
}

#[test]
fn boolean_on_tessellation_states_loss() {
    let tol = TessTol::default();
    let slab = |x0: f64, y0: f64, x1: f64, y1: f64, z: f64, t: f64| {
        let a = Curve::polyline(&[v3(x0, y0, z), v3(x1, y0, z)]).unwrap();
        let b = Curve::polyline(&[v3(x0, y1, z), v3(x1, y1, z)]).unwrap();
        thicken(&ruled(&a, &b).unwrap(), t, &tol).unwrap()
    };
    let a = slab(0., 0., 2., 3., 0., 0.1);
    let b = slab(0.5, 0.5, 1.5, 1.5, -1., 2.);
    let (m, loss) = boolean_on_tessellation(&a, &b, BoolOp::Subtract, Tol::default());
    assert!((m.volume() - 0.5).abs() < 1e-6, "{}", m.volume());
    assert_eq!(loss.len(), 1);
    assert!(loss[0].measure <= tol.chord);
}

#[test]
fn large_freeform_boolean_takes_the_mesh_path() {
    let tol = TessTol::default();
    let prof = Curve::arc(O, Z, X, 1.0, 0.0, FRAC_PI_2).unwrap();
    let dome = thicken(&revolve(&prof, O, Z, TAU).unwrap(), 0.1, &tol).unwrap();
    let c0 = Curve::circle(v3(0.2, 0.1, -0.5), X, Y, 0.5).unwrap();
    let wall = loft(&[c0.clone(), c0.translated(v3(0., 0., 2.))], 1)
        .unwrap()
        .surface;
    let wall = thicken(&wall, 0.1, &tol).unwrap();
    let n = dome.mesh.triangles.len() + wall.mesh.triangles.len();
    assert!(n >= crate::csg2::MIN_FACES, "{n}");
    let direct =
        crate::csg2::mesh_boolean(&dome.mesh, &wall.mesh, BoolOp::Subtract, Tol::default());
    assert!(direct.is_ok(), "{:?}", direct.err());
    let (s, loss) = boolean_on_tessellation(&dome, &wall, BoolOp::Subtract, Tol::default());
    let (i, _) = boolean_on_tessellation(&dome, &wall, BoolOp::Intersect, Tol::default());
    assert!(s.is_watertight() && i.is_watertight());
    assert!(i.volume() > 1e-3);
    assert!((s.volume() + i.volume() - dome.mesh.volume()).abs() < 1e-9);
    assert!(s.face_of.iter().all(|f| *f == 1 || *f == 2));
    assert_eq!(loss.len(), 1);
}

#[test]
fn rejects_invalid_input() {
    let p = vec![v3(0., 0., 0.), v3(1., 0., 0.), v3(2., 1., 0.)];
    assert!(matches!(
        Curve::bspline(2, vec![0., 0., 0., 1., 1.], p.clone()),
        Err(NurbsError::KnotCount { .. })
    ));
    assert_eq!(
        Curve::new(2, vec![0., 0., 0., 1., 1., 1.], p.clone(), vec![1., 0., 1.]),
        Err(NurbsError::BadWeight(1))
    );
    assert_eq!(
        Curve::bspline(2, vec![0., 0., 0.5, 1., 1., 1.], p.clone()),
        Err(NurbsError::NotClamped)
    );
    assert_eq!(
        Curve::bspline(2, vec![0., 0., 0., 1., 0.5, 1.], p.clone()),
        Err(NurbsError::BadKnots)
    );
    assert_eq!(Curve::arc(O, X, Y, 1.0, 1.0, 1.0), Err(NurbsError::BadAngle));
    assert_eq!(Curve::arc(O, X, X, 1.0, 0.0, 1.0), Err(NurbsError::Degenerate));
    assert_eq!(Curve::polyline(&[O, O]), Err(NurbsError::Degenerate));
    let a = Curve::polyline(&[O, X]).unwrap();
    let flat = ruled(&a, &a.translated(Y)).unwrap();
    assert_eq!(
        thicken(&flat, 0.0, &TessTol::default()).unwrap_err(),
        NurbsError::BadThickness
    );
}

#[test]
fn loft_through_reproduces_curves_at_the_given_parameters() {
    let rows: Vec<Curve> = (0..4)
        .map(|j| {
            let z = j as f64;
            Curve::interpolate(&[v3(0., 0., z), v3(1., 0.3 * z, z), v3(2., 0., z + 0.2), v3(3., 0.1, z)], 3).unwrap()
        })
        .collect();
    let params = [0.0, 0.2, 0.7, 1.0];
    let lf = loft_through(&rows, 3, &params).unwrap();
    assert_eq!(lf.params, params);
    for (k, c) in rows.iter().enumerate() {
        for i in 0..=20 {
            let u = i as f64 / 20.0;
            assert!(dist(lf.surface.point(u, params[k]), c.point(u)) <= 1e-9, "curve {k} at {u}");
        }
    }
}

#[test]
fn loft_through_gives_neighbours_one_edge_where_loft_does_not() {
    // two patches over the same column of points, each with other points elsewhere
    let col = |x: f64, z: f64, y: f64| v3(x, y * z * 0.1, z);
    let patch = |x0: f64, x1: f64, bend: f64| -> Vec<Curve> {
        [0.0, 1.0, 2.5, 3.0]
            .iter()
            .map(|&z| {
                Curve::interpolate(&[col(x0, z, 1.0), col(0.5 * (x0 + x1), z, bend), col(x1, z, 1.0)], 2).unwrap()
            })
            .collect()
    };
    let (a, b) = (patch(0.0, 1.0, 5.0), patch(1.0, 2.0, 1.0));
    let params = [0.0, 0.3, 0.8, 1.0];
    let edge = |s: &Surface, u: f64| (0..=20).map(|i| s.point(u, i as f64 / 20.0)).collect::<Vec<_>>();
    let (sa, sb) = (loft_through(&a, 3, &params).unwrap().surface, loft_through(&b, 3, &params).unwrap().surface);
    let gap = edge(&sa, 1.0).iter().zip(edge(&sb, 0.0)).map(|(p, q)| dist(*p, q)).fold(0.0, f64::max);
    assert!(gap <= 1e-9, "shared edge differs by {gap}");
    let (la, lb) = (loft(&a, 3).unwrap(), loft(&b, 3).unwrap());
    let own = (0..=20)
        .map(|i| {
            let (va, vb) = (i as f64 / 20.0, i as f64 / 20.0);
            dist(la.surface.point(1.0, va), lb.surface.point(0.0, vb))
        })
        .fold(0.0, f64::max);
    assert!(own > 1e-6, "chord parameters are expected to differ here: {own}");
}

#[test]
fn loft_through_refuses_bad_parameters() {
    let c = Curve::polyline(&[O, X]).unwrap();
    let rows = [c.clone(), c.clone(), c];
    for bad in [&[0.0, 1.0][..], &[0.0, 0.5, 0.5], &[0.1, 0.5, 1.0], &[0.0, 0.5, 0.9], &[0.0, 0.8, 0.5]] {
        assert!(loft_through(&rows, 2, bad).is_err(), "{bad:?}");
    }
    assert!(loft_through(&rows[..1], 2, &[0.0]).is_err());
}
