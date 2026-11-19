use super::*;
use crate::math::{v2, v3, V2, V3};
use crate::predicates::point_in_ring;

/// Tilted plane: origin and orthonormal axes, so the projection axis is not trivial.
fn frame() -> (V3, V3, V3) {
    let e1 = v3(1.0, 2.0, 0.5).normalized();
    let t = v3(-0.3, 0.4, 1.0);
    let e2 = (t - e1 * t.dot(e1)).normalized();
    (v3(10.0, -5.0, 3.0), e1, e2)
}

fn ring(p: &[(f64, f64)]) -> Vec<V2> {
    p.iter().map(|&(x, y)| v2(x, y)).collect()
}

/// Outer counter-clockwise, holes clockwise in the plane frame.
fn check(outer: Vec<V2>, holes: Vec<Vec<V2>>) {
    let (o, e1, e2) = frame();
    let mut pos = vec![];
    let mut loops = vec![];
    for r in std::iter::once(&outer).chain(&holes) {
        let mut l = vec![];
        for p in r {
            l.push(pos.len() as u32);
            pos.push(o + e1 * p.x + e2 * p.y);
        }
        loops.push(l);
    }
    let tris = fill_loops(&pos, &loops).expect("region fills");
    let flat = |i: u32| {
        let d = pos[i as usize] - o;
        v2(d.dot(e1), d.dot(e2))
    };
    let inside =
        |p: V2| point_in_ring(p, &outer) >= 0 && holes.iter().all(|h| point_in_ring(p, h) <= 0);
    let n = e1.cross(e2);
    let want = signed_area(&outer) + holes.iter().map(|h| signed_area(h)).sum::<f64>();
    let mut got = 0.0;
    for t in &tris {
        let [a, b, c] = t.map(|i| pos[i as usize]);
        let cr = (b - a).cross(c - a);
        assert!(cr.dot(n) >= -1e-12, "folded {t:?}");
        got += cr.len() * 0.5;
        if cr.len() > 1e-12 {
            let [p, q, r] = t.map(flat);
            let third = 1.0 / 3.0;
            for (wa, wb) in [
                (third, third),
                (0.8, 0.1),
                (0.1, 0.8),
                (0.1, 0.1),
                (0.45, 0.45),
                (0.45, 0.1),
                (0.1, 0.45),
            ] {
                let s = p * wa + q * wb + r * (1.0 - wa - wb);
                assert!(inside(s), "{t:?} leaves the polygon at {s:?}");
            }
        }
    }
    assert!((got - want).abs() < 1e-6, "area {got} vs {want}");
}

#[test]
fn l_shaped_non_convex_face() {
    let l = ring(&[
        (0.0, 0.0),
        (4.0, 0.0),
        (4.0, 1.0),
        (1.0, 1.0),
        (1.0, 3.0),
        (0.0, 3.0),
    ]);
    check(l, vec![]);
}

#[test]
fn face_with_holes() {
    let outer = ring(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
    let h1 = ring(&[(1.0, 1.0), (1.0, 2.0), (2.0, 2.0), (2.0, 1.0)]);
    let h2 = ring(&[(2.5, 2.5), (2.5, 3.5), (3.5, 3.5), (3.5, 2.5)]);
    check(outer, vec![h1, h2]);
}

#[test]
fn collinear_points_on_a_u_shape() {
    let u = ring(&[
        (0.0, 0.0),
        (1.0, 0.0),
        (2.0, 0.0),
        (3.0, 0.0),
        (3.0, 1.0),
        (3.0, 2.0),
        (3.0, 3.0),
        (2.0, 3.0),
        (2.0, 2.0),
        (2.0, 1.0),
        (1.0, 1.0),
        (1.0, 2.0),
        (1.0, 3.0),
        (0.0, 3.0),
        (0.0, 2.0),
        (0.0, 1.0),
    ]);
    check(u, vec![]);
}

#[test]
fn duplicate_points() {
    let l = ring(&[
        (0.0, 0.0),
        (0.0, 0.0),
        (4.0, 0.0),
        (4.0, 1.0),
        (4.0, 1.0),
        (1.0, 1.0),
        (1.0, 3.0),
        (0.0, 3.0),
        (0.0, 3.0),
    ]);
    check(l, vec![]);
}

#[test]
fn non_planar_loop_is_left_alone() {
    let pos = vec![
        v3(0.0, 0.0, 0.0),
        v3(1.0, 0.0, 0.0),
        v3(1.0, 1.0, 0.5),
        v3(0.0, 1.0, 0.0),
    ];
    assert!(fill_loops(&pos, &[vec![0, 1, 2, 3]]).is_none());
}

/// A wall body far taller than its sloped top (imported walls cut by a roof): slice points
/// inside the side faces used to keep their old height and fold the faces into spikes.
#[test]
fn tall_wall_under_low_sloped_top_has_no_spikes() {
    let tol = crate::Tol::default();
    let plan = vec![v2(0.0, -0.1), v2(4.5, -0.1), v2(4.5, 0.1), v2(0.0, 0.1)];
    let p = crate::Profile::new(plan.clone(), vec![], tol).unwrap();
    let mut m = crate::Solid::extrude(&p, 8.0, tol).triangulate().unwrap();
    let g = [(0.00003, 4.0), (0.1, 4.04), (0.2, 4.06), (4.3, 4.06), (4.4, 4.03)];
    let (a, dir) = (v2(0.0, 0.0), v2(1.0, 0.0));
    crate::top::apply(&mut m, a, dir, 0.0, 8.0, &g);
    assert!(m.is_watertight());
    for t in &m.triangles {
        for &i in t {
            let z = m.positions[i as usize].z;
            assert!(z <= 4.06 + 1e-9, "vertex at {z} above the top");
        }
    }
    // each side face: nothing turned over, area = area under the top
    let want = crate::top::area_under(&g, 0.0, 4.5, 0.0, 100.0);
    for side in [-0.1, 0.1] {
        let mut area = 0.0;
        for t in &m.triangles {
            let [p, q, r] = t.map(|i| m.positions[i as usize]);
            if [p, q, r].iter().all(|v| (v.y - side).abs() < 1e-9) {
                let n = (q - p).cross(r - p);
                assert!(n.y * side >= -1e-12, "folded side triangle {t:?}");
                area += n.len() * 0.5;
            }
        }
        assert!((area - want).abs() < 1e-9, "side {side}: {area} vs {want}");
    }
    let q = crate::top::integral(&plan, a, dir, &g);
    assert!((m.volume() - q).abs() < 1e-9, "{} vs {q}", m.volume());
}

/// Sloped top: every side-face vertex is on the side plane and sits either on the base or
/// exactly on the top polyline; the side face area equals the area under the top.
#[test]
fn sloped_top_side_faces_planar_and_match_top_edge() {
    let tol = crate::Tol::default();
    let plan = vec![v2(0.0, -0.1), v2(4.0, -0.1), v2(4.0, 0.1), v2(0.0, 0.1)];
    let p = crate::Profile::new(plan, vec![], tol).unwrap();
    let mut m = crate::Solid::extrude(&p, 6.0, tol).triangulate().unwrap();
    let g = [(0.0, 3.0), (2.0, 3.5), (4.0, 4.0)];
    crate::top::apply(&mut m, v2(0.0, 0.0), v2(1.0, 0.0), 0.0, 6.0, &g);
    assert!(m.is_watertight());
    let top_at = |u: f64| 3.0 + 0.25 * u;
    let mut side_pts = 0;
    for t in &m.triangles {
        let [p, q, r] = t.map(|i| m.positions[i as usize]);
        for side in [-0.1, 0.1] {
            if [p, q, r].iter().all(|v| (v.y - side).abs() < 1e-9) {
                for v in [p, q, r] {
                    side_pts += 1;
                    assert!(
                        v.z.abs() < 1e-9 || (v.z - top_at(v.x)).abs() < 1e-9,
                        "side vertex {v:?} neither on base nor on the top edge"
                    );
                }
            }
        }
    }
    assert!(side_pts > 0);
}
