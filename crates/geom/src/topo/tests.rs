// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::math::{v2, v3};

fn rng(seed: &mut u64) -> f64 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((*seed >> 11) as f64) / ((1u64 << 53) as f64)
}

fn grid(n: usize, step: f64, f: impl Fn(f64, f64) -> f64) -> Vec<V3> {
    let mut v = vec![];
    for j in 0..n {
        for i in 0..n {
            let (x, y) = (i as f64 * step, j as f64 * step);
            v.push(v3(x, y, f(x, y)));
        }
    }
    v
}

fn build(points: &[V3]) -> Tin {
    Tin::build(&TinInput {
        points,
        ..Default::default()
    })
    .unwrap()
}

fn ring_area(r: &[V2]) -> f64 {
    (0..r.len())
        .map(|i| r[i].cross(r[(i + 1) % r.len()]))
        .sum::<f64>()
        * 0.5
}

fn hull_area(pts: &[V3]) -> f64 {
    let mut p: Vec<V2> = pts.iter().map(|q| v2(q.x, q.y)).collect();
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    let mut h: Vec<V2> = vec![];
    for pass in 0..2 {
        let base = h.len();
        let it: Vec<V2> = if pass == 0 {
            p.clone()
        } else {
            p.iter().rev().copied().collect()
        };
        for x in it {
            while h.len() >= base + 2 && orient2d(h[h.len() - 2], h[h.len() - 1], x) <= 0.0 {
                h.pop();
            }
            h.push(x);
        }
        h.pop();
    }
    ring_area(&h)
}

#[test]
fn random_points_valid_delaunay_and_hull() {
    let mut s = 7u64;
    let pts: Vec<V3> = (0..2000)
        .map(|_| v3(rng(&mut s) * 500.0, rng(&mut s) * 300.0, rng(&mut s) * 20.0))
        .collect();
    let t = build(&pts);
    t.validate().unwrap();
    assert_eq!(t.pts.len(), 2000);
    assert!((t.plan_area() - hull_area(&pts)).abs() < 1e-6 * hull_area(&pts));
    assert!(t.src.iter().all(|&i| (i as usize) < pts.len()));
}

#[test]
fn far_coordinates_valid() {
    let mut s = 11u64;
    let pts: Vec<V3> = (0..500)
        .map(|_| {
            v3(
                6.5e6 + rng(&mut s) * 100.0,
                4.2e5 + rng(&mut s) * 100.0,
                150.0 + rng(&mut s),
            )
        })
        .collect();
    build(&pts).validate().unwrap();
}

#[test]
fn regular_grid_cocircular_valid() {
    let pts = grid(30, 1.0, |x, _| x * 0.1);
    let t = build(&pts);
    t.validate().unwrap();
    assert_eq!(t.tris.len(), 2 * 29 * 29);
    assert!((t.plan_area() - 29.0 * 29.0).abs() < 1e-9);
}

#[test]
fn degenerate_inputs_rejected() {
    let two = [v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0)];
    assert_eq!(
        Tin::build(&TinInput {
            points: &two,
            ..Default::default()
        })
        .unwrap_err(),
        TinError::TooFewPoints
    );
    let line: Vec<V3> = (0..10).map(|i| v3(i as f64, 2.0 * i as f64, 0.0)).collect();
    assert_eq!(
        Tin::build(&TinInput {
            points: &line,
            ..Default::default()
        })
        .unwrap_err(),
        TinError::Collinear
    );
    let dup = [v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), v3(0.0, 0.0, 2.0)];
    assert!(Tin::build(&TinInput {
        points: &dup,
        ..Default::default()
    })
    .is_err());
    let bad = [v3(0.0, 0.0, 0.0), v3(f64::NAN, 0.0, 0.0), v3(1.0, 1.0, 0.0)];
    assert_eq!(
        Tin::build(&TinInput {
            points: &bad,
            ..Default::default()
        })
        .unwrap_err(),
        TinError::BadCoord
    );
}

/// The constrained edge a–b lies on the segment p–q.
fn on(t: &Tin, e: [u32; 2], p: V2, q: V2) -> bool {
    let (a, b) = (t.pts[e[0] as usize], t.pts[e[1] as usize]);
    let near = |x: V3| {
        let d = q - p;
        let w = v2(x.x, x.y) - p;
        (d.cross(w) / d.len()).abs() < 1e-9 && w.dot(d) >= -1e-9 && w.dot(d) <= d.dot(d) + 1e-9
    };
    near(a) && near(b)
}

#[test]
fn crossing_breaklines_get_steiner_point() {
    let mut s = 3u64;
    let pts: Vec<V3> = (0..400)
        .map(|_| v3(rng(&mut s) * 100.0, rng(&mut s) * 100.0, 0.0))
        .collect();
    let l1 = vec![v3(5.0, 5.0, 0.0), v3(95.0, 95.0, 9.0)];
    let l2 = vec![v3(5.0, 95.0, 2.0), v3(95.0, 5.0, 2.0)];
    let t = Tin::build(&TinInput {
        points: &pts,
        breaklines: &[l1.clone(), l2.clone()],
        ..Default::default()
    })
    .unwrap();
    t.validate().unwrap();
    // both lines are built from constrained edges, and their lengths equal the originals
    for l in [&l1, &l2] {
        let (p, q) = (v2(l[0].x, l[0].y), v2(l[1].x, l[1].y));
        let len: f64 = t
            .fixed
            .iter()
            .filter(|&&e| on(&t, e, p, q))
            .map(|e| {
                let (a, b) = (t.pts[e[0] as usize], t.pts[e[1] as usize]);
                (a.x - b.x).hypot(a.y - b.y)
            })
            .sum();
        assert!((len - (q - p).len()).abs() < 1e-6, "{len}");
    }
    // the intersection point lies on the first line and takes its elevation (4.5)
    let x = t
        .pts
        .iter()
        .find(|p| (p.x - 50.0).abs() < 1e-9 && (p.y - 50.0).abs() < 1e-9)
        .expect("steiner");
    assert!((x.z - 4.5).abs() < 1e-9);
}

#[test]
fn concave_boundary_clips() {
    let pts = grid(11, 1.0, |x, y| x + y);
    let l = vec![
        v2(0.0, 0.0),
        v2(10.0, 0.0),
        v2(10.0, 4.0),
        v2(4.0, 4.0),
        v2(4.0, 10.0),
        v2(0.0, 10.0),
    ];
    let t = Tin::build(&TinInput {
        points: &pts,
        boundary: Some(l.as_slice()),
        ..Default::default()
    })
    .unwrap();
    t.validate().unwrap();
    assert!((t.plan_area() - ring_area(&l)).abs() < 1e-9);
    // the sloped plane z = x + y is preserved
    assert!((t.z_at(v2(2.5, 7.5)).unwrap() - 10.0).abs() < 1e-9);
    assert!(t.z_at(v2(7.0, 7.0)).is_none());
}

#[test]
fn boundary_off_grid_is_draped() {
    let pts = grid(11, 1.0, |x, _| 2.0 * x);
    let b = vec![v2(0.5, 0.3), v2(9.7, 1.1), v2(8.2, 9.4), v2(1.3, 8.8)];
    let t = Tin::build(&TinInput {
        points: &pts,
        boundary: Some(b.as_slice()),
        ..Default::default()
    })
    .unwrap();
    t.validate().unwrap();
    assert!((t.plan_area() - ring_area(&b)).abs() < 1e-9);
    for p in &t.pts {
        assert!((p.z - 2.0 * p.x).abs() < 1e-9, "{p:?}");
    }
}

#[test]
fn regions_follow_outline() {
    let pts = grid(21, 0.5, |x, y| (x * 0.3).sin() + y * 0.05);
    let r = vec![v2(2.2, 2.1), v2(6.3, 2.4), v2(5.9, 7.7), v2(2.6, 6.1)];
    let t = Tin::build(&TinInput {
        points: &pts,
        regions: std::slice::from_ref(&r),
        ..Default::default()
    })
    .unwrap();
    t.validate().unwrap();
    let inside: f64 = t
        .tris
        .iter()
        .zip(&t.region_of)
        .filter(|&(_, &k)| k == 0)
        .map(|(tr, _)| t.tri_area(tr))
        .sum();
    assert!((inside - ring_area(&r)).abs() < 1e-9, "{inside}");
    let again = t.with_regions(&[r.clone()]).unwrap();
    again.validate().unwrap();
    assert!((again.plan_area() - t.plan_area()).abs() < 1e-9);
}

#[test]
fn volume_to_plane_matches_analytic() {
    // z = 0.1·x on [0, 10]²: relative to the plane 0.5, cut = fill = 10·∫(0.1x − 0.5)⁺ = 12.5
    let t = build(&grid(11, 1.0, |x, _| 0.1 * x));
    let cf = t.volume_to_plane(0.5);
    assert!((cf.cut - 12.5).abs() < 1e-9, "{cf:?}");
    assert!((cf.fill - 12.5).abs() < 1e-9, "{cf:?}");
    // plane whose zero line bends inside triangles
    let t = build(&grid(7, 1.7, |x, y| 0.3 * x - 0.2 * y + 1.0));
    let cf = t.volume_to_plane(1.3);
    let total: f64 = t
        .tris
        .iter()
        .map(|tr| {
            let z = tr.map(|i| t.pts[i as usize].z - 1.3);
            t.tri_area(tr) * (z[0] + z[1] + z[2]) / 3.0
        })
        .sum();
    assert!((cf.cut - cf.fill - total).abs() < 1e-9);
}

#[test]
fn contours_on_plane() {
    let t = build(&grid(11, 1.0, |x, _| 0.1 * x));
    let cs = t.contours(0.25, 2);
    for z in [0.25, 0.5, 0.75] {
        let at: Vec<&Contour> = cs.iter().filter(|c| (c.z - z).abs() < 1e-12).collect();
        assert_eq!(at.len(), 1, "level {z}");
        let c = at[0];
        assert!(!c.closed);
        assert_eq!(c.major, z == 0.5);
        for p in &c.pts {
            assert!((p.x - z / 0.1).abs() < 1e-9, "{p:?}");
            assert_eq!(p.z, z);
        }
        let ys: Vec<f64> = c.pts.iter().map(|p| p.y).collect();
        let (lo, hi) = ys
            .iter()
            .fold((f64::MAX, f64::MIN), |a, &y| (a.0.min(y), a.1.max(y)));
        assert!(lo.abs() < 1e-9 && (hi - 10.0).abs() < 1e-9);
    }
    assert!(cs.iter().all(|c| c.z < 1.0));
}

#[test]
fn contours_on_cone_are_closed_rings() {
    let mut pts = vec![v3(0.0, 0.0, 0.0)];
    for r in 1..=6 {
        for k in 0..48 {
            let a = k as f64 * std::f64::consts::TAU / 48.0;
            pts.push(v3(r as f64 * a.cos(), r as f64 * a.sin(), r as f64));
        }
    }
    let t = build(&pts);
    t.validate().unwrap();
    let cs = t.contours(0.5, 2);
    let at: Vec<&Contour> = cs.iter().filter(|c| (c.z - 2.5).abs() < 1e-12).collect();
    assert_eq!(at.len(), 1);
    let c = at[0];
    assert!(c.closed);
    assert!(c.pts.len() >= 48);
    for p in &c.pts {
        let r = p.x.hypot(p.y);
        assert!(r <= 2.5 + 1e-9 && r >= 2.4, "{r}");
    }
    assert!(!label_spots(c, 5.0).is_empty());
}

#[test]
fn solid_is_watertight_prism() {
    let mut s = 5u64;
    let pts: Vec<V3> = (0..300)
        .map(|_| v3(rng(&mut s) * 40.0, rng(&mut s) * 30.0, rng(&mut s) * 3.0))
        .collect();
    let t = build(&pts);
    let m = t.solid(1.5);
    assert!(m.is_watertight());
    assert!((m.volume() - t.plan_area() * 1.5).abs() < 1e-6 * m.volume());
    assert_eq!(m.face_of.len(), m.triangles.len());
}

#[test]
fn profile_and_poche_on_plane() {
    let t = build(&grid(11, 1.0, |x, _| 0.1 * x));
    let pr = t.profile(v2(-5.0, 5.0), v2(15.0, 5.0));
    assert_eq!(pr.len(), 1);
    let p = &pr[0];
    assert!((p[0].x - 5.0).abs() < 1e-9 && p[0].y.abs() < 1e-9);
    let l = p[p.len() - 1];
    assert!((l.x - 15.0).abs() < 1e-9 && (l.y - 1.0).abs() < 1e-9);
    for q in p {
        assert!((q.y - 0.1 * (q.x - 5.0)).abs() < 1e-9);
    }
    let rings = t.poche(v2(-5.0, 5.0), v2(15.0, 5.0), 0.5);
    assert_eq!(rings.len(), 1);
    assert!((ring_area(&rings[0]) - 5.0).abs() < 1e-9);
}

fn flat_site() -> Tin {
    build(&grid(61, 1.0, |_, _| 0.0))
}

fn square(x0: f64, y0: f64, w: f64) -> Vec<V2> {
    vec![
        v2(x0, y0),
        v2(x0 + w, y0),
        v2(x0 + w, y0 + w),
        v2(x0, y0 + w),
    ]
}

#[test]
fn pad_cut_matches_analytic_frustum() {
    // excavation of depth h with side slope m over a convex outline: A·h + P·m·h²/2 + π·m²·h³/3
    let ex = flat_site();
    let (h, m) = (1.0, 1.0);
    let pad = Pad {
        outline: square(25.0, 25.0, 10.0),
        z: -h,
        slope: m,
    };
    let g = grade(&ex, &[pad]).unwrap();
    let want = 100.0 * h + 40.0 * m * h * h / 2.0 + std::f64::consts::PI * m * m * h * h * h / 3.0;
    assert!(
        (g.cut[0] - want).abs() < 1e-3 * want,
        "{} vs {want}",
        g.cut[0]
    );
    assert!(g.fill[0] < 1e-9);
    g.tin.validate().unwrap();
    // the zero-work line is 1 m from the outline
    for p in &g.daylight[0] {
        let dx = (p.x - 30.0).abs() - 5.0;
        let dy = (p.y - 30.0).abs() - 5.0;
        let d = if dx > 0.0 && dy > 0.0 {
            dx.hypot(dy)
        } else {
            dx.max(dy)
        };
        assert!((d - 1.0).abs() < 1e-6, "{d}");
    }
    // graded surface: pad floor at the target elevation, original terrain outside
    assert!((g.tin.z_at(v2(30.0, 30.0)).unwrap() + h).abs() < 1e-9);
    assert!(g.tin.z_at(v2(5.0, 5.0)).unwrap().abs() < 1e-9);
    assert!(g.pad_tris.iter().any(|&k| k == 0));
}

#[test]
fn pad_fill_and_vertical_sides() {
    let ex = flat_site();
    let g = grade(
        &ex,
        &[Pad {
            outline: square(10.0, 10.0, 10.0),
            z: 2.0,
            slope: 1.5,
        }],
    )
    .unwrap();
    let (h, m) = (2.0, 1.5);
    let want = 100.0 * h + 40.0 * m * h * h / 2.0 + std::f64::consts::PI * m * m * h * h * h / 3.0;
    assert!(
        (g.fill[0] - want).abs() < 1e-3 * want,
        "{} vs {want}",
        g.fill[0]
    );
    assert!(g.cut[0] < 1e-9);
    let g = grade(
        &ex,
        &[Pad {
            outline: square(30.0, 30.0, 8.0),
            z: -0.5,
            slope: 0.0,
        }],
    )
    .unwrap();
    assert!((g.cut[0] - 32.0).abs() < 0.01, "{}", g.cut[0]);
}

#[test]
fn smooth_lowers_spike() {
    let mut pts = grid(9, 1.0, |_, _| 0.0);
    pts[40].z = 5.0;
    let t = build(&pts);
    let v = t.src.iter().position(|&s| s == 40).unwrap();
    let z = smooth(&t, &square(2.5, 2.5, 3.0), 3);
    assert!(z[v] < 2.0 && z[v] > 0.0);
    let far = t.src.iter().position(|&s| s == 0).unwrap();
    assert_eq!(z[far], 0.0);
}

fn seg_dist_t(p: V2, a: V2, b: V2) -> f64 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.dot(ab)).clamp(0.0, 1.0);
    (p - (a + ab * t)).len()
}

fn dist_to_ring(r: &[V2], p: V2) -> f64 {
    (0..r.len())
        .map(|i| seg_dist_t(p, r[i], r[(i + 1) % r.len()]))
        .fold(f64::INFINITY, f64::min)
}

/// Independent pad volume on flat terrain z = 0: h·A inside and ∫(h − d/m)⁺ outside
/// (grid cell midpoints).
fn pad_volume_numeric(ring: &[V2], h: f64, m: f64, cell: f64) -> f64 {
    let reach = h * m;
    let (mut lo, mut hi) = (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN));
    for p in ring {
        lo = v2(lo.x.min(p.x), lo.y.min(p.y));
        hi = v2(hi.x.max(p.x), hi.y.max(p.y));
    }
    let (x0, y0) = (lo.x - reach - cell, lo.y - reach - cell);
    let nx = ((hi.x - lo.x + 2.0 * (reach + cell)) / cell).ceil() as usize;
    let ny = ((hi.y - lo.y + 2.0 * (reach + cell)) / cell).ceil() as usize;
    let mut v = 0.0;
    for j in 0..ny {
        for i in 0..nx {
            let p = v2(x0 + (i as f64 + 0.5) * cell, y0 + (j as f64 + 0.5) * cell);
            v += if point_in_ring(p, ring) > 0 {
                h
            } else {
                (h - dist_to_ring(ring, p) / m).max(0.0)
            };
        }
    }
    v * cell * cell
}

/// Ring without self-intersections (checks all pairs of non-adjacent edges).
fn ring_is_simple(r: &[V2]) -> bool {
    let n = r.len();
    let opp = |a: f64, b: f64| (a > 0.0 && b < 0.0) || (a < 0.0 && b > 0.0);
    for i in 0..n {
        for j in i + 1..n {
            if j == i + 1 || (j + 1) % n == i {
                continue;
            }
            let (a, b, c, d) = (r[i], r[(i + 1) % n], r[j], r[(j + 1) % n]);
            if opp(orient2d(a, b, c), orient2d(a, b, d))
                && opp(orient2d(c, d, a), orient2d(c, d, b))
            {
                return false;
            }
        }
    }
    true
}

fn check_concave_pad(outline: Vec<V2>) {
    let ex = flat_site();
    let m = 1.5;
    for z in [2.0_f64, -2.0] {
        let h: f64 = z.abs();
        let g = grade(
            &ex,
            &[Pad {
                outline: outline.clone(),
                z,
                slope: m,
            }],
        )
        .unwrap();
        g.tin.validate().unwrap();
        let dl = &g.daylight[0];
        assert!(dl.len() >= 8);
        assert!(ring_is_simple(dl), "daylight self-intersects (z = {z})");
        assert!(ring_area(dl) > ring_area(&outline));
        // the zero-work line is at distance h·m from the outline (outline offset)
        for p in dl {
            let d = dist_to_ring(&outline, *p);
            assert!((d - h * m).abs() < 1e-6, "z = {z}: {p:?} at {d}");
            assert!(point_in_ring(*p, &outline) < 0);
        }
        for p in &outline {
            assert!(point_in_ring(*p, dl) > 0);
        }
        let want = pad_volume_numeric(&outline, h, m, 0.025);
        let (got, other) = if z > 0.0 {
            (g.fill[0], g.cut[0])
        } else {
            (g.cut[0], g.fill[0])
        };
        assert!((got - want).abs() < 0.01 * want, "z = {z}: {got} vs {want}");
        assert!(other < 1e-6 * want, "z = {z}: {other}");
        // on the slope the elevation is z ∓ d/m (including at a reentrant corner and in a bay)
        for (x, y) in [(27.0, 27.0), (17.0, 20.0), (17.0, 25.0), (28.5, 27.2)] {
            let p = v2(x, y);
            if point_in_ring(p, &outline) >= 0 {
                continue;
            }
            let d = dist_to_ring(&outline, p);
            if d >= h * m {
                continue;
            }
            let want_z = z - z.signum() * d / m;
            let got_z = g.tin.z_at(p).unwrap();
            assert!(
                (got_z - want_z).abs() < 0.05,
                "z = {z} at {p:?}: {got_z} vs {want_z}"
            );
        }
    }
}

#[test]
fn l_shaped_pad_concave_corner() {
    // reentrant corner (26, 26); the 3 m slope exceeds the distance from neighboring rays to the corner
    check_concave_pad(vec![
        v2(20.0, 20.0),
        v2(32.0, 20.0),
        v2(32.0, 26.0),
        v2(26.0, 26.0),
        v2(26.0, 32.0),
        v2(20.0, 32.0),
    ]);
}

#[test]
fn u_shaped_pad_narrow_bay() {
    // 4 m wide bay with a 3 m slope: the bay lies entirely under the slope, axis x = 17
    check_concave_pad(vec![
        v2(10.0, 10.0),
        v2(24.0, 10.0),
        v2(24.0, 26.0),
        v2(19.0, 26.0),
        v2(19.0, 15.0),
        v2(15.0, 15.0),
        v2(15.0, 26.0),
        v2(10.0, 26.0),
    ]);
}

#[test]
fn contours_through_vertices() {
    // levels exactly on vertex rows: one open polyline per level, no gaps
    let t = build(&grid(11, 1.0, |x, _| x));
    let cs = t.contours(1.0, 5);
    for k in 1..10 {
        let z = k as f64;
        let at: Vec<&Contour> = cs.iter().filter(|c| (c.z - z).abs() < 1e-12).collect();
        assert_eq!(at.len(), 1, "level {z}");
        assert!(!at[0].closed);
        for p in &at[0].pts {
            assert!((p.x - z).abs() < 1e-9, "{p:?}");
        }
        let (lo, hi) = at[0]
            .pts
            .iter()
            .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.y), a.1.max(p.y)));
        assert!(
            lo.abs() < 1e-9 && (hi - 10.0).abs() < 1e-9,
            "level {z}: {lo}..{hi}"
        );
    }
}

#[test]
fn long_collinear_breakline_fully_fixed() {
    // breakline through a row of 400 survey points: all its segments are constraint edges
    let n = 400;
    let mut pts = vec![];
    for i in 0..n {
        for j in [-1.0, 0.0, 1.0] {
            pts.push(v3(i as f64, j, if j == 0.0 { 1.0 } else { 0.0 }));
        }
    }
    let line = vec![v3(0.0, 0.0, 1.0), v3((n - 1) as f64, 0.0, 1.0)];
    let t = Tin::build(&TinInput {
        points: &pts,
        breaklines: &[line],
        ..Default::default()
    })
    .unwrap();
    t.validate().unwrap();
    let on_line = t
        .fixed
        .iter()
        .filter(|e| e.iter().all(|&v| t.pts[v as usize].y.abs() < 1e-12))
        .count();
    assert_eq!(on_line, n - 1);
}

#[test]
fn near_duplicates_across_cell_boundary_merge() {
    // 0.2 µm on opposite sides of a rounding-cell boundary: one vertex
    let pts = vec![
        v3(0.0, 0.0, 0.0),
        v3(10.0, 0.0, 0.0),
        v3(0.0, 10.0, 0.0),
        v3(3.0 + 0.4e-6, 3.0, 1.0),
        v3(3.0 + 0.6e-6, 3.0, 1.0),
    ];
    let t = build(&pts);
    assert_eq!(t.pts.len(), 4);
}
