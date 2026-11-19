// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::csg::Body;
use crate::layered::{join_with, layer_footprints, Layer};
use crate::math::Tol;

fn strip(a: V2, b: V2, t: f64, h: f64) -> Strip {
    Strip {
        a,
        b,
        layers: vec![Layer::new(t)],
        offset0: -t / 2.0,
        base: 0.0,
        height: h,
    }
}

fn rect(st: &Strip) -> Vec<V2> {
    let n = st.normal();
    let bo = st.bounds();
    let (s0, s1) = (bo[0], bo[bo.len() - 1]);
    vec![st.a + n * s0, st.b + n * s0, st.b + n * s1, st.a + n * s1]
}

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * b.abs().max(1.0)
}

#[test]
fn lean_10_deg_volume_is_plan_area_times_height() {
    let st = strip(v2(0.0, 0.0), v2(6.0, 0.0), 0.3, 3.0);
    let l = Lean::new(10f64.to_radians(), 1.0);
    let base = rect(&st);
    let top = top_ring(&st, 0.0, &l, &base, None);
    let want = 6.0 * 0.3 * 3.0;
    assert!(close(loft_volume(&base, &top, 3.0), want, 1e-12));
    let m = loft(&base, &top, 0.0, 3.0);
    assert!(m.is_watertight());
    assert!(close(m.volume(), want, 1e-12));
    // the top face moved by h * tan(10 deg) toward the left normal
    let sh = 3.0 * 10f64.to_radians().tan();
    assert!((top[0].y - (base[0].y + sh)).abs() < 1e-12);
}

#[test]
fn taper_volume_is_the_frustum_of_the_section() {
    let st = strip(v2(0.0, 0.0), v2(4.0, 0.0), 0.6, 5.0);
    let l = Lean::new(0.0, 0.5);
    let base = rect(&st);
    let top = top_ring(&st, 0.0, &l, &base, None);
    // trapezoid section: (0.6 + 0.3) / 2 * 5, times the length
    let want = 4.0 * (0.6 + 0.3) / 2.0 * 5.0;
    assert!(close(loft_volume(&base, &top, 5.0), want, 1e-12));
    assert!(close(l.prism_volume(4.0 * 0.6, 5.0), want, 1e-12));
    assert!(close(loft(&base, &top, 0.0, 5.0).volume(), want, 1e-12));
    // lean and taper together: shear keeps the volume
    let lt = Lean::new(-15f64.to_radians(), 0.5);
    let top2 = top_ring(&st, 0.0, &lt, &base, None);
    assert!(close(loft_volume(&base, &top2, 5.0), want, 1e-12));
}

/// Square tower, every wall leaning inward: joins at the base and at the top give pieces whose
/// volumes add up to the analytic shell, so corners neither gap nor overlap.
#[test]
fn inward_tower_corners_are_closed() {
    let (lw, h, t) = (10.0, 6.0, 0.3);
    let th = 8f64.to_radians();
    let p = [v2(0.0, 0.0), v2(lw, 0.0), v2(lw, lw), v2(0.0, lw)];
    let strips: Vec<Strip> = (0..4).map(|i| strip(p[i], p[(i + 1) % 4], t, h)).collect();
    let leans = vec![Lean::new(th, 1.0); 4];
    let tops = top_strips(&strips, &[0.0; 4], &leans).0;
    let tol = Tol::default();
    let prefs = vec![[None, None]; 4];
    let jb = join_with(&strips, &prefs, tol).unwrap();
    let jt = join_with(&tops, &prefs, tol).unwrap();
    assert!(jb.fillers.is_empty() && jt.fillers.is_empty());
    let mut vol = 0.0;
    for i in 0..4 {
        let fb = layer_footprints(&strips[i], &jb.pieces[i], i, tol).unwrap();
        let ft = layer_footprints(&tops[i], &jt.pieces[i], i, tol).unwrap();
        for (b, tp) in fb[0].iter().zip(&ft[0]) {
            let mapped = mapped_ring(&strips[i], 0.0, &leans[i], &b.outer);
            let paired = pair_rings(&mapped, &tp.outer).expect("top pairs with base");
            vol += loft_volume(&b.outer, &paired, h);
            assert!(loft(&b.outer, &paired, 0.0, h).is_watertight());
        }
    }
    let tn = th.tan();
    let want = 2.0 * t * h * (2.0 * lw - 2.0 * tn * h);
    assert!(close(vol, want, 1e-9), "{vol} vs {want}");
}

/// The same tower cut at 1.2 m: the joined section is the square ring shrunk by z tan(lean)
/// on every side; a vertical neighbour keeps its line and meets the leaning wall's section.
#[test]
fn section_of_leaning_walls_at_a_cut_height() {
    let (lw, h, t, z) = (10.0, 6.0, 0.3, 1.2);
    let th = 8f64.to_radians();
    let p = [v2(0.0, 0.0), v2(lw, 0.0), v2(lw, lw), v2(0.0, lw)];
    let strips: Vec<Strip> = (0..4).map(|i| strip(p[i], p[(i + 1) % 4], t, h)).collect();
    let tol = Tol::default();
    let prefs = vec![[None, None]; 4];
    let area = |leans: &[Lean]| -> f64 {
        let cut = section_strips(&strips, &[0.0; 4], leans, z).0;
        let j = join_with(&cut, &prefs, tol).unwrap();
        assert!(j.fillers.is_empty());
        (0..4)
            .flat_map(|i| layer_footprints(&cut[i], &j.pieces[i], i, tol).unwrap().remove(0))
            .map(|f| poly::area(&f.outer).abs())
            .sum()
    };
    let sh = z * th.tan();
    let all = area(&[Lean::new(th, 1.0); 4]);
    assert!(close(all, 4.0 * t * (lw - 2.0 * sh), 1e-9), "{all}");
    // only the first wall leans: its section moves in by sh, the two side walls shorten by sh
    let one = area(&[Lean::new(th, 1.0), Lean::PLAIN, Lean::PLAIN, Lean::PLAIN]);
    assert!(close(one, 4.0 * t * lw - 2.0 * t * sh, 1e-9), "{one}");
    // a section above every wall's top is the top join
    let tops = top_strips(&strips, &[0.0; 4], &[Lean::new(th, 1.0); 4]).0;
    let above = section_strips(&strips, &[0.0; 4], &[Lean::new(th, 1.0); 4], 50.0).0;
    for (a, b) in tops.iter().zip(&above) {
        assert!((a.a - b.a).len() < 1e-12 && (a.b - b.b).len() < 1e-12 && a.base == b.base);
    }
}

#[test]
fn door_in_leaning_wall_is_cut_normal_and_volume_matches() {
    let tol = Tol::default();
    let st = strip(v2(0.0, 0.0), v2(5.0, 0.0), 0.3, 3.0);
    let l = Lean::new(12f64.to_radians(), 1.0);
    let base = rect(&st);
    let top = top_ring(&st, 0.0, &l, &base, None);
    let wall = loft(&base, &top, 0.0, 3.0);
    let door = Opening {
        along: 1.5,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
    };
    let cutter = opening_cutter(&st, 0.0, &l, &door);
    assert!(cutter.is_watertight() && cutter.volume() > 0.0);
    // the head face (2 of the 4 vertices at the top of the cutter) contains the surface normal
    let (up, nn) = surface_frame(st.normal(), &l);
    let head: Vec<V3> = cutter.positions.iter().copied().filter(|q| q.dot(up) > 2.0).collect();
    assert_eq!(head.len(), 4);
    for q in &head {
        assert!((q.dot(up) - head[0].dot(up)).abs() < 1e-9);
    }
    assert!(nn.dot(up).abs() < 1e-12);
    let cut = Body::from_mesh(&wall, 0).subtract(&Body::from_mesh(&cutter, 1), tol).to_mesh();
    let want = wall.volume() - door.width * cut_area(&l, 3.0, -0.15, 0.15, 0.0, 2.1);
    assert!(close(cut.volume(), want, 1e-6), "{} vs {want}", cut.volume());
    // an inner window of a lean-only wall: its size along the surface times the thickness
    // normal to the surface
    let win = cut_area(&l, 3.0, -0.15, 0.15, 0.9, 1.2);
    assert!(close(win, 1.2 * 0.3 * l.angle.cos(), 1e-12));
    // the fill turned about the sill line lands on the cutter's edges
    let w = Opening {
        along: 1.0,
        width: 1.0,
        sill: 0.9,
        height: 1.2,
    };
    let c = opening_cutter(&st, 0.0, &l, &w);
    let top_mid = tilt(&st, 0.0, &l, v3(1.0, 0.0, 0.9 + 1.2), 0.9);
    let (up2, _) = surface_frame(st.normal(), &l);
    let vmax = c.positions.iter().map(|q| q.dot(up2)).fold(f64::MIN, f64::max);
    assert!((top_mid.dot(up2) - vmax).abs() < 1e-9);
}

#[test]
fn window_in_tapered_leaning_wall_volume_matches_csg() {
    let tol = Tol::default();
    let st = strip(v2(0.0, 0.0), v2(4.0, 0.0), 0.5, 4.0);
    let l = Lean::new(-9f64.to_radians(), 0.6);
    let base = rect(&st);
    let top = top_ring(&st, 0.0, &l, &base, None);
    let wall = loft(&base, &top, 0.0, 4.0);
    let w = Opening {
        along: 1.0,
        width: 1.2,
        sill: 1.0,
        height: 1.5,
    };
    let cut = Body::from_mesh(&wall, 0)
        .subtract(&Body::from_mesh(&opening_cutter(&st, 0.0, &l, &w), 1), tol)
        .to_mesh();
    let want = wall.volume() - openings_volume(&l, 4.0, -0.25, 0.25, &[w], None);
    assert!(close(cut.volume(), want, 1e-6), "{} vs {want}", cut.volume());
}

#[test]
fn strip_at_is_the_horizontal_section() {
    let st = strip(v2(0.0, 0.0), v2(5.0, 0.0), 0.4, 4.0);
    let l = Lean::new(20f64.to_radians(), 0.5);
    let s2 = strip_at(&st, 0.0, &l, 2.0);
    let tot: f64 = s2.layers.iter().map(|x| x.thickness).sum();
    assert!((tot - 0.4 * 0.75).abs() < 1e-12);
    let p = l.map(&st, 0.0, v2(1.0, -0.2), 2.0);
    assert!((p.y - (s2.a.y + s2.offset0)).abs() < 1e-12);
    assert!(Lean::new(0.0, 1.0).is_plain() && !l.is_plain() && l.valid());
    assert!(!Lean::new(1.2, 1.0).valid() && !Lean::new(0.0, 0.0).valid());
}

/// Quarter circle of radius 5 about the origin, turning left, from (5, 0) to (0, 5).
const QUARTER: f64 = 0.414_213_562_373_095_1; // tan(pi / 8)

/// The layer ring of the quarter-circle wall between left offsets -t/2 and t/2.
fn arc_ring(t: f64) -> BulgeRing {
    let (ro, ri) = (5.0 + t / 2.0, 5.0 - t / 2.0);
    BulgeRing {
        pts: vec![v2(ro, 0.0), v2(0.0, ro), v2(0.0, ri), v2(ri, 0.0)],
        bulges: vec![QUARTER, 0.0, -QUARTER, 0.0],
    }
}

/// Volume of the leaning quarter-circle layer: section area pi/4 (ro^2 - ri^2) = pi/2 t k (r - sh)
/// integrated over z (quadratic: Simpson is exact), independent of the ring code.
fn arc_volume(l: &Lean, t: f64, h: f64) -> f64 {
    let f = |z: f64| std::f64::consts::FRAC_PI_2 * t * l.k(z, h) * (5.0 - l.shift(z));
    h / 6.0 * (f(0.0) + 4.0 * f(h / 2.0) + f(h))
}

#[test]
fn arc_wall_leans_by_the_radial_map() {
    let (t, h) = (0.4, 3.0);
    let st = strip(v2(5.0, 0.0), v2(0.0, 5.0), t, h);
    let l = Lean::new(10f64.to_radians(), 0.8);
    // the axis section at z is the concentric arc of radius r - shift (the axis turns left)
    let s2 = strip_at(&st, QUARTER, &l, 2.0);
    assert!((s2.a.len() - (5.0 - l.shift(2.0))).abs() < 1e-12);
    assert!((s2.b.len() - (5.0 - l.shift(2.0))).abs() < 1e-12 && s2.b.x.abs() < 1e-12);
    // the outer face (s = -t/2) goes to radius r - (s k + shift)
    let p = l.map(&st, QUARTER, v2(0.0, 5.0 + t / 2.0), h);
    let want = 5.0 - (-t / 2.0 * l.top_scale + l.shift(h));
    assert!((p.len() - want).abs() < 1e-12 && p.x.abs() < 1e-12);
    // volume: Simpson on the exact rings equals the analytic integral
    let base = arc_ring(t);
    let top = top_bulge_ring(&st, QUARTER, &l, &base, None);
    let v = arc_volume(&l, t, h);
    assert!(close(ring_volume(&base, &top, h), v, 1e-12), "{v}");
    // the mesh is the same solid within the chord sag
    let tol = Tol::default();
    let ms = ring_meshes(&st, QUARTER, &l, &[vec![base]], None, &[], tol);
    assert_eq!(ms.len(), 1);
    assert!(ms[0].1.is_watertight());
    assert!(close(ms[0].1.volume(), v, 2e-3), "{} vs {v}", ms[0].1.volume());
}

#[test]
fn door_in_leaning_arc_wall_matches_the_cut() {
    let (t, h) = (0.4, 3.0);
    let st = strip(v2(5.0, 0.0), v2(0.0, 5.0), t, h);
    let l = Lean::new(-8f64.to_radians(), 1.0);
    let door = Opening {
        along: 2.5,
        width: 1.0,
        sill: 0.0,
        height: 2.1,
    };
    let cutter = opening_cutter(&st, QUARTER, &l, &door);
    assert!(cutter.is_watertight() && cutter.volume() > 0.0);
    let tol = Tol::default();
    let base = vec![vec![arc_ring(t)]];
    let whole = ring_meshes(&st, QUARTER, &l, &base, None, &[], tol)[0].1.volume();
    let cut = ring_meshes(&st, QUARTER, &l, &base, None, &[door], tol)[0].1.volume();
    let removed = openings_volume(&l, h, -t / 2.0, t / 2.0, &[door], arc_of(&st, QUARTER));
    assert!(close(whole - cut, removed, 2e-3), "{} vs {removed}", whole - cut);
    // the opening's section is turned about the centre: more removed outside than inside
    let flat = openings_volume(&l, h, -t / 2.0, t / 2.0, &[door], None);
    assert!(removed > 0.0 && (removed - flat).abs() > 1e-6);
    // a fill point turns within its radial plane
    let q = tilt(&st, QUARTER, &l, v3(3.0, 4.0, 1.5), 0.0);
    assert!(v2(q.x, q.y).cross(v2(3.0, 4.0)).abs() < 1e-12);
}

#[test]
fn arc_and_straight_wall_meet_at_the_top() {
    let (t, h) = (0.3, 4.0);
    let th = 6f64.to_radians();
    let strips = vec![
        strip(v2(5.0, 0.0), v2(0.0, 5.0), t, h),
        strip(v2(0.0, 5.0), v2(0.0, 9.0), t, h),
    ];
    let bulges = [QUARTER, 0.0];
    let leans = [Lean::new(th, 1.0); 2];
    let (tops, tb) = top_strips(&strips, &bulges, &leans);
    let sh = h * th.tan();
    // the node: on the arc's concentric circle and on the straight wall's shifted line
    let q = tops[0].b;
    assert!((q.len() - (5.0 - sh)).abs() < 1e-9 && (q.x + sh).abs() < 1e-9, "{q:?}");
    assert!((tops[1].a - q).len() < 1e-9);
    // the arc keeps its centre with the new bulge
    let ax = ArcAxis::new(tops[0].a, tops[0].b, tb[0], Tol::default()).unwrap();
    assert!(ax.center.len() < 1e-9 && (ax.radius - (5.0 - sh)).abs() < 1e-9);
    assert_eq!(tb[1], 0.0);
}
