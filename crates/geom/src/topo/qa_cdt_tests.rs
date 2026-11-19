//! QA: constrained Delaunay triangulation helpers.

use super::cdt::{Cdt, SUPER};
use crate::math::{v2, V2};

fn area2(a: V2, b: V2, c: V2) -> f64 {
    (b - a).cross(c - a)
}

fn grid_cdt(n: usize) -> (Cdt, Vec<u32>) {
    let mut c = Cdt::new(v2(0.0, 0.0), v2(n as f64, n as f64));
    let mut ids = vec![];
    for i in 0..n {
        for j in 0..n {
            // a slight deterministic wobble keeps four points off one circle
            let w = ((i * 7 + j * 13) % 5) as f64 * 1e-3;
            let v = c.push(v2(i as f64 + w, j as f64 - w), (i + j) as f64);
            ids.push(c.insert(v));
        }
    }
    (c, ids)
}

#[test]
fn push_numbers_vertices_after_the_super_triangle() {
    let mut c = Cdt::new(v2(0.0, 0.0), v2(1.0, 1.0));
    let a = c.push(v2(0.2, 0.2), 1.0);
    let b = c.push(v2(0.8, 0.3), 2.0);
    assert_eq!((a, b), (SUPER, SUPER + 1));
    assert!(!c.inserted(a), "pushed is not yet inserted");
    assert_eq!(c.p[a as usize], v2(0.2, 0.2));
    assert_eq!(c.z[b as usize], 2.0);
    assert_eq!(c.insert(a), a);
    assert!(c.inserted(a));
    assert!(!c.inserted(b));
    assert!(!c.inserted(9999), "unknown vertex");
}

#[test]
fn insert_of_a_coincident_point_returns_the_old_vertex() {
    let mut c = Cdt::new(v2(0.0, 0.0), v2(1.0, 1.0));
    let a = c.push(v2(0.5, 0.5), 0.0);
    c.insert(a);
    let dup = c.push(v2(0.5, 0.5), 9.0);
    assert_eq!(c.insert(dup), a);
    assert!(!c.inserted(dup));
}

#[test]
fn triangulation_covers_the_hull_and_is_oriented() {
    let (c, ids) = grid_cdt(6);
    assert_eq!(ids.len(), 36);
    let hull = c.hull();
    assert!(hull.len() >= 4);
    let hp: Vec<V2> = hull.iter().map(|&i| c.p[i as usize]).collect();
    let hull_area: f64 = (0..hp.len()).map(|i| hp[i].cross(hp[(i + 1) % hp.len()])).sum::<f64>() / 2.0;
    assert!(hull_area > 0.0, "counter-clockwise hull");
    let out = c.finish(None);
    assert!(out.tris.iter().all(|t| t.iter().all(|&v| (v as usize) < out.p.len())));
    let total: f64 = out
        .tris
        .iter()
        .map(|t| {
            let a = area2(out.p[t[0] as usize], out.p[t[1] as usize], out.p[t[2] as usize]);
            assert!(a > 0.0, "every triangle is counter-clockwise");
            a / 2.0
        })
        .sum();
    // thin slivers along the hull may be dropped with the super triangle, never added
    assert!(total <= hull_area + 1e-9, "{total} vs {hull_area}");
    assert!(total > 0.99 * hull_area, "{total} vs {hull_area}");
    assert!(out.tris.len() <= 2 * 36 - 2 - hull.len());
}

#[test]
fn constrain_forces_an_edge_and_finish_reports_it() {
    let (mut c, ids) = grid_cdt(5);
    let (a, b) = (ids[0], ids[ids.len() - 1]);
    c.constrain(a, b);
    let out = c.finish(None);
    assert!(!out.fixed.is_empty());
    // the constrained chain runs from the first to the last vertex without leaving the segment
    let pa = out.p[out.old.iter().position(|&o| o == a).unwrap()];
    let pb = out.p[out.old.iter().position(|&o| o == b).unwrap()];
    for e in &out.fixed {
        for &v in e {
            let p = out.p[v as usize];
            assert!(area2(pa, pb, p).abs() < 1e-6 * (pb - pa).len().max(1.0), "{p:?}");
        }
    }
}

#[test]
fn finish_with_a_clip_drops_outside_triangles() {
    let (c, _) = grid_cdt(6);
    let clip = [v2(-0.5, -0.5), v2(2.5, -0.5), v2(2.5, 2.5), v2(-0.5, 2.5)];
    let out = c.finish(Some(&clip));
    assert!(!out.tris.is_empty());
    for t in &out.tris {
        let m = (out.p[t[0] as usize] + out.p[t[1] as usize] + out.p[t[2] as usize]) * (1.0 / 3.0);
        assert!(m.x < 2.6 && m.y < 2.6, "{m:?}");
    }
}

#[test]
fn empty_and_tiny_inputs() {
    let c = Cdt::new(v2(0.0, 0.0), v2(0.0, 0.0));
    assert!(c.hull().len() < 3);
    assert!(c.finish(None).tris.is_empty());
    let mut one = Cdt::new(v2(0.0, 0.0), v2(1.0, 1.0));
    let a = one.push(v2(0.5, 0.5), 0.0);
    one.insert(a);
    assert!(one.finish(None).tris.is_empty(), "a single point makes no triangle");
}
