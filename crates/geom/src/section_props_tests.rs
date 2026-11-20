// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * b.abs().max(1e-300)
}

macro_rules! near {
    ($a:expr, $b:expr, $rel:expr) => {{
        let (a, b) = ($a, $b);
        assert!(close(a, b, $rel), "{} = {a}, expected {b} (rel {})", stringify!($a), $rel);
    }};
}

/// Composite of axis-aligned rectangles (y0, z0, y1, z1) by the parallel-axis theorem:
/// (A, cy, cz, Iy, Iz, Iyz) about the centroid. Independent of the polygon integrals.
fn rects(rs: &[(f64, f64, f64, f64)]) -> (f64, f64, f64, f64, f64, f64) {
    let parts: Vec<(f64, f64, f64, f64, f64)> = rs
        .iter()
        .map(|&(y0, z0, y1, z1)| {
            let (b, h) = (y1 - y0, z1 - z0);
            (b * h, (y0 + y1) / 2.0, (z0 + z1) / 2.0, b, h)
        })
        .collect();
    let a: f64 = parts.iter().map(|p| p.0).sum();
    let cy = parts.iter().map(|p| p.0 * p.1).sum::<f64>() / a;
    let cz = parts.iter().map(|p| p.0 * p.2).sum::<f64>() / a;
    let (mut iy, mut iz, mut iyz) = (0.0, 0.0, 0.0);
    for &(ar, y, z, b, h) in &parts {
        iy += b * h.powi(3) / 12.0 + ar * (z - cz).powi(2);
        iz += h * b.powi(3) / 12.0 + ar * (y - cy).powi(2);
        iyz += ar * (y - cy) * (z - cz);
    }
    (a, cy, cz, iy, iz, iyz)
}

fn rings(s: &Shape) -> (Vec<V2>, Vec<Vec<V2>>) {
    let (o, h) = s.rings();
    (o, h.into_iter().collect())
}

fn grid(s: &Shape) -> SectionProps {
    let (o, h) = rings(s);
    polygon_with(&o, &h, 60, 6.0)
}

fn ngon(r: f64, n: usize) -> Vec<V2> {
    (0..n)
        .map(|k| {
            let a = 2.0 * PI * k as f64 / n as f64;
            v2(r * a.cos(), r * a.sin())
        })
        .collect()
}

fn rot(r: &[V2], a: f64) -> Vec<V2> {
    let (c, s) = (a.cos(), a.sin());
    r.iter().map(|p| v2(c * p.x - s * p.y, s * p.x + c * p.y)).collect()
}

const I300: Shape = Shape::I {
    h: 0.3,
    b: 0.15,
    tw: 0.0071,
    tf: 0.0107,
};

#[test]
fn rectangle_closed_forms() {
    let (b, h) = (0.3, 0.5);
    let p = of_shape(&Shape::Rect { b, h });
    let a = b * h;
    near!(p.area, a, 1e-12);
    near!(p.iy, b * h.powi(3) / 12.0, 1e-12);
    near!(p.iz, h * b.powi(3) / 12.0, 1e-12);
    assert!(p.iyz.abs() < 1e-15);
    near!(p.wy, b * h * h / 6.0, 1e-12);
    near!(p.wz, h * b * b / 6.0, 1e-12);
    near!(p.wpl_y, b * h * h / 4.0, 1e-9);
    near!(p.wpl_z, h * b * b / 4.0, 1e-9);
    near!(p.avy, 5.0 / 6.0 * a, 1e-9);
    near!(p.avz, 5.0 / 6.0 * a, 1e-9);
    near!(p.ry, h / 12f64.sqrt(), 1e-12);
    near!(p.i1, p.iy, 1e-12);
    assert!(p.alpha.abs() < 1e-12);
    assert_eq!(p.torsion, TorsionMethod::Exact);
    assert!(p.centroid.len() < 1e-15 && p.shear_center.len() < 1e-15);
    // tabulated Saint-Venant factors: square 0.1406 a^4, h/b = 2: 0.2287 h b^3
    near!(rect_torsion(0.2, 0.2) / 0.2f64.powi(4), 0.140_577, 1e-4);
    near!(rect_torsion(0.1, 0.2) / (0.2 * 0.1f64.powi(3)), 0.228_682, 1e-4);
    near!(rect_torsion(0.2, 0.1), rect_torsion(0.1, 0.2), 1e-15);
    // thin strip tends to b t^3 / 3 (1 - 0.630 t / b)
    near!(rect_torsion(1.0, 0.01), 0.01f64.powi(3) / 3.0 * (1.0 - 0.630 * 0.01), 1e-4);
}

#[test]
fn circle_and_pipe_closed_forms() {
    let d = 0.4;
    let p = of_shape(&Shape::Circle { d });
    let a = PI * d * d / 4.0;
    near!(p.area, a, 1e-12);
    near!(p.iy, PI * d.powi(4) / 64.0, 1e-12);
    near!(p.iz, p.iy, 1e-12);
    near!(p.j, PI * d.powi(4) / 32.0, 1e-12);
    near!(p.wy, PI * d.powi(3) / 32.0, 1e-12);
    near!(p.wpl_y, d.powi(3) / 6.0, 1e-12);
    near!(p.avz, 0.9 * a, 1e-12);
    near!(p.rz, d / 4.0, 1e-12);

    let (d, t) = (0.3, 0.01);
    let di = d - 2.0 * t;
    let p = of_shape(&Shape::Pipe { d, t });
    let a = PI * (d * d - di * di) / 4.0;
    let i = PI * (d.powi(4) - di.powi(4)) / 64.0;
    near!(p.area, a, 1e-12);
    near!(p.iy, i, 1e-12);
    near!(p.j, 2.0 * i, 1e-12);
    near!(p.wz, i / (d / 2.0), 1e-12);
    near!(p.wpl_z, (d.powi(3) - di.powi(3)) / 6.0, 1e-12);
    // Jourawski on a thin ring tends to 2/3 A
    let thin = of_shape(&Shape::Pipe { d, t: 0.0015 });
    near!(thin.avz / thin.area, 2.0 / 3.0, 5e-3);
    near!(thin.avy, thin.avz, 1e-6);
    assert!(p.avz / a > 0.66 && p.avz / a < 0.70);
}

#[test]
fn hollow_rectangle_closed_forms() {
    let (h, b, t) = (0.3, 0.2, 0.01);
    let (bi, hi) = (b - 2.0 * t, h - 2.0 * t);
    let p = of_shape(&Shape::Tube { h, b, t });
    near!(p.area, b * h - bi * hi, 1e-12);
    near!(p.iy, (b * h.powi(3) - bi * hi.powi(3)) / 12.0, 1e-12);
    near!(p.iz, (h * b.powi(3) - hi * bi.powi(3)) / 12.0, 1e-12);
    near!(p.wy, (b * h.powi(3) - bi * hi.powi(3)) / (6.0 * h), 1e-12);
    near!(p.wpl_y, (b * h * h - bi * hi * hi) / 4.0, 1e-9);
    near!(p.wpl_z, (h * b * b - hi * bi * bi) / 4.0, 1e-9);
    let (bm, hm) = (b - t, h - t);
    near!(p.j, 4.0 * (bm * hm).powi(2) * t / (2.0 * (bm + hm)), 1e-12);
    assert_eq!(p.torsion, TorsionMethod::ThinWalled);
    // shear along z is carried by the webs
    assert!(p.avz > 2.0 * hi * t && p.avz < 2.0 * h * t * 1.2);
}

#[test]
fn i_section_closed_forms() {
    let (h, b, tw, tf) = (0.3, 0.15, 0.0071, 0.0107);
    let hw = h - 2.0 * tf;
    let p = of_shape(&I300);
    near!(p.area, 2.0 * b * tf + hw * tw, 1e-12);
    near!(p.iy, (b * h.powi(3) - (b - tw) * hw.powi(3)) / 12.0, 1e-12);
    near!(p.iz, (2.0 * tf * b.powi(3) + hw * tw.powi(3)) / 12.0, 1e-12);
    near!(p.wy, p.iy / (h / 2.0), 1e-12);
    near!(p.wz, p.iz / (b / 2.0), 1e-12);
    near!(p.wpl_y, b * tf * (h - tf) + tw * hw * hw / 4.0, 1e-9);
    near!(p.wpl_z, tf * b * b / 2.0 + hw * tw * tw / 4.0, 1e-9);
    // Jourawski integral of the I evaluated independently by adaptive quadrature
    near!(p.avz, 0.002_095_324_588_350_05, 1e-9);
    assert!(p.avz > hw * tw && p.avz < h * tw);
    near!(p.j, (2.0 * b * tf.powi(3) + hw * tw.powi(3)) / 3.0, 1e-12);
    assert!(p.shear_center.len() < 1e-15 && p.centroid.len() < 1e-12);
}

#[test]
fn channel_centroid_and_shear_center() {
    let (h, b, tw, tf) = (0.3, 0.1, 0.0071, 0.011);
    let (y, z) = (b / 2.0, h / 2.0);
    let s = Shape::Channel { h, b, tw, tf };
    let p = of_shape(&s);
    let c = rects(&[(-y, -z, y, -z + tf), (-y, z - tf, y, z), (-y, -z + tf, -y + tw, z - tf)]);
    near!(p.area, c.0, 1e-12);
    near!(p.centroid.x, c.1, 1e-9);
    assert!(p.centroid.y.abs() < 1e-12);
    near!(p.iy, c.3, 1e-12);
    near!(p.iz, c.4, 1e-12);
    near!(p.wz, c.4 / (y - c.1), 1e-12);
    let (bf, hw) = (b - tw / 2.0, h - tf);
    let e = 3.0 * bf * bf * tf / (6.0 * bf * tf + hw * tw);
    near!(p.shear_center.x, -y + tw / 2.0 - e, 1e-12);
    // shear centre lies outside the web, away from the flanges
    assert!(p.shear_center.x < -y);
    let g = grid(&s);
    assert!((g.shear_center.x - p.shear_center.x).abs() < 0.01 * b, "{:?}", g.shear_center);
    assert!(g.shear_center.y.abs() < 1e-6);
    near!(g.j, p.j, 0.05);
}

#[test]
fn tee_centroid_plastic_and_shear_center() {
    let (h, b, tw, tf) = (0.2, 0.2, 0.02, 0.02);
    let (y, z) = (b / 2.0, h / 2.0);
    let s = Shape::Tee { h, b, tw, tf };
    let p = of_shape(&s);
    let c = rects(&[(-y, z - tf, y, z), (-tw / 2.0, -z, tw / 2.0, z - tf)]);
    near!(p.area, c.0, 1e-12);
    near!(p.centroid.y, c.2, 1e-9);
    near!(p.iy, c.3, 1e-12);
    near!(p.wy, c.3 / (c.2 + z), 1e-12);
    // equal-area axis inside the flange, depth d from the top
    let d = c.0 / (2.0 * b);
    let web = (h - tf) * tw;
    let wpl = b * d * d / 2.0 + b * (tf - d).powi(2) / 2.0 + web * ((h - tf) / 2.0 + tf - d);
    near!(p.wpl_y, wpl, 1e-9);
    assert_eq!((p.shear_center.x, p.shear_center.y), (0.0, z - tf / 2.0));
    let g = grid(&s);
    assert!((g.shear_center.y - p.shear_center.y).abs() < 0.02 * h);
    near!(g.j, p.j, 0.05);
}

#[test]
fn angle_principal_axes_and_shear_center() {
    let (h, b, t) = (0.1, 0.1, 0.01);
    let (y, z) = (b / 2.0, h / 2.0);
    let s = Shape::Angle { h, b, t };
    let p = of_shape(&s);
    let c = rects(&[(-y, -z, y, -z + t), (-y, -z + t, -y + t, z)]);
    near!(p.area, (b + h - t) * t, 1e-12);
    near!(p.centroid.x, c.1, 1e-9);
    near!(p.centroid.y, c.2, 1e-9);
    near!(p.iy, c.3, 1e-12);
    near!(p.iyz, c.5, 1e-12);
    assert!(p.iyz < 0.0);
    // equal legs: principal axes at 45 degrees, major along the heel-to-toes diagonal
    near!(p.alpha, PI / 4.0, 1e-9);
    near!(p.i1, c.3 - c.5, 1e-9);
    near!(p.i2, c.3 + c.5, 1e-9);
    assert_eq!((p.shear_center.x, p.shear_center.y), (-y + t / 2.0, -z + t / 2.0));
    let g = grid(&s);
    assert!((g.shear_center - p.shear_center).len() < 0.05 * b, "{:?}", g.shear_center);
    near!(g.j, p.j, 0.05);
    let un = of_shape(&Shape::Angle { h: 0.15, b: 0.1, t: 0.012 });
    assert!(un.alpha > 0.0 && un.alpha < PI / 4.0);
}

#[test]
fn polygon_method_matches_parametric_shapes() {
    let shapes = [
        (Shape::Rect { b: 0.3, h: 0.5 }, 0.01),
        (Shape::Tube { h: 0.3, b: 0.2, t: 0.01 }, 0.03),
        (I300, 0.05),
        (Shape::Channel { h: 0.3, b: 0.1, tw: 0.0071, tf: 0.011 }, 0.05),
        (Shape::Tee { h: 0.2, b: 0.2, tw: 0.02, tf: 0.02 }, 0.05),
        (Shape::Angle { h: 0.15, b: 0.1, t: 0.012 }, 0.05),
    ];
    for (s, jtol) in &shapes {
        let (p, g) = (of_shape(s), grid(s));
        assert_eq!(g.torsion, TorsionMethod::Grid);
        for (x, y) in [
            (g.area, p.area),
            (g.iy, p.iy),
            (g.iz, p.iz),
            (g.wy, p.wy),
            (g.wz, p.wz),
            (g.wpl_y, p.wpl_y),
            (g.wpl_z, p.wpl_z),
            (g.avy, p.avy),
            (g.avz, p.avz),
        ] {
            assert!(close(x, y, 1e-12), "{s:?}: {x} vs {y}");
        }
        assert!(close(g.j, p.j, *jtol), "{s:?}: J {} vs {}", g.j, p.j);
    }
    // doubly symmetric: grid shear centre on the centroid
    let g = grid(&I300);
    assert!(g.shear_center.len() < 1e-6);
}

#[test]
fn polygon_method_matches_round_sections() {
    let d = 0.4;
    let c = of_shape(&Shape::Circle { d });
    let p = polygon_with(&ngon(d / 2.0, 128), &[], 60, 6.0);
    near!(p.area, c.area, 2e-3);
    near!(p.iy, c.iy, 3e-3);
    near!(p.wpl_y, c.wpl_y, 2e-3);
    near!(p.avz, c.avz, 2e-3);
    near!(p.j, c.j, 0.02);
    assert!(p.shear_center.len() < 1e-3 * d);

    let (d, t) = (0.4, 0.05);
    let c = of_shape(&Shape::Pipe { d, t });
    let mut hole = ngon(d / 2.0 - t, 128);
    hole.reverse();
    let p = polygon_with(&ngon(d / 2.0, 128), &[hole.clone()], 60, 6.0);
    near!(p.area, c.area, 2e-3);
    near!(p.avz, c.avz, 3e-3);
    near!(p.j, c.j, 0.02);
    // hole orientation does not matter
    hole.reverse();
    let q = polygon_with(&ngon(d / 2.0, 128), &[hole], 60, 6.0);
    near!(q.area, p.area, 1e-12);
    near!(q.j, p.j, 1e-9);
}

#[test]
fn principal_axes_are_rotation_invariant() {
    let (o, _) = rings(&I300);
    // default resolution: thin sloped walls are the documented 5 % case
    let p0 = polygon(&o, &[]);
    for th in [0.4, 1.1, -0.7] {
        let p = polygon(&rot(&o, th), &[]);
        near!(p.area, p0.area, 1e-12);
        near!(p.i1, p0.iy, 1e-9);
        near!(p.i2, p0.iz, 1e-9);
        near!(p.iy + p.iz, p0.iy + p0.iz, 1e-12);
        // axis of i1 is the rotated y axis (mod pi)
        let da = (p.alpha - th).rem_euclid(PI);
        assert!(da < 1e-9 || PI - da < 1e-9, "alpha {} for {th}", p.alpha);
        near!(p.j, p0.j, 0.055);
    }
    // angle: principal values keep, the axis turns with the section
    let (o, _) = rings(&Shape::Angle { h: 0.15, b: 0.1, t: 0.012 });
    let a0 = polygon_with(&o, &[], 30, 3.0);
    let a1 = polygon_with(&rot(&o, 0.3), &[], 30, 3.0);
    near!(a1.i1, a0.i1, 1e-9);
    near!(a1.i2, a0.i2, 1e-9);
    let da = (a1.alpha - a0.alpha - 0.3).rem_euclid(PI);
    assert!(da < 1e-9 || PI - da < 1e-9);
    // compact rectangle: J barely depends on the grid orientation
    let (r, _) = rings(&Shape::Rect { b: 0.3, h: 0.5 });
    // any off-grid angle; 0.5236 is a sample, not pi/6
    #[allow(clippy::approx_constant)]
    let j = polygon_with(&rot(&r, 0.5236), &[], 60, 6.0).j;
    near!(j, rect_torsion(0.3, 0.5), 0.03);
    // a quarter turn maps the grid onto itself: shear centre turns exactly
    let (c, _) = rings(&Shape::Channel { h: 0.3, b: 0.1, tw: 0.0071, tf: 0.011 });
    let g0 = polygon_with(&c, &[], 40, 4.0);
    let g1 = polygon_with(&rot(&c, PI / 2.0), &[], 40, 4.0);
    assert!((g1.shear_center - v2(-g0.shear_center.y, g0.shear_center.x)).len() < 1e-6);
    near!(g1.j, g0.j, 1e-6);
    near!(g1.avy, g0.avz, 1e-9);
}

#[test]
fn units_scale_by_powers_of_length() {
    let k: f64 = 1000.0; // m -> mm
    let Shape::I { h, b, tw, tf } = I300 else { unreachable!() };
    let m = of_shape(&I300);
    let mm = of_shape(&Shape::I {
        h: h * k,
        b: b * k,
        tw: tw * k,
        tf: tf * k,
    });
    near!(mm.area, m.area * k.powi(2), 1e-12);
    near!(mm.avz, m.avz * k.powi(2), 1e-9);
    near!(mm.iy, m.iy * k.powi(4), 1e-12);
    near!(mm.j, m.j * k.powi(4), 1e-12);
    near!(mm.wy, m.wy * k.powi(3), 1e-12);
    near!(mm.wpl_z, m.wpl_z * k.powi(3), 1e-9);
    near!(mm.ry, m.ry * k, 1e-12);
    // the grid scales with the section, so the numerical J scales exactly too
    let (o, _) = rings(&Shape::Angle { h: 0.15, b: 0.1, t: 0.012 });
    let so: Vec<V2> = o.iter().map(|p| v2(p.x * k, p.y * k)).collect();
    let (g, gs) = (polygon_with(&o, &[], 30, 3.0), polygon_with(&so, &[], 30, 3.0));
    near!(gs.j, g.j * k.powi(4), 1e-6);
    near!(gs.shear_center.x, g.shear_center.x * k, 1e-6);
}

#[test]
fn orientation_and_degenerate_input() {
    let (o, _) = rings(&I300);
    let mut cw = o.clone();
    cw.reverse();
    let (a, b) = (polygon_with(&o, &[], 30, 3.0), polygon_with(&cw, &[], 30, 3.0));
    near!(b.area, a.area, 1e-12);
    near!(b.wpl_y, a.wpl_y, 1e-12);
    near!(b.j, a.j, 1e-9);
    let z = polygon(&[v2(0.0, 0.0), v2(1.0, 0.0)], &[]);
    assert_eq!(z, SectionProps::default());
    let flat = polygon(&[v2(0.0, 0.0), v2(1.0, 0.0), v2(2.0, 0.0)], &[]);
    assert_eq!(flat.area, 0.0);
    // a free polygon goes through the numerical method
    let p = of_shape(&Shape::Poly(vec![(0.0, 0.0), (0.2, 0.0), (0.2, 0.4), (0.0, 0.4)]));
    assert_eq!(p.torsion, TorsionMethod::Grid);
    near!(p.j, rect_torsion(0.2, 0.4), 0.01);
    assert!(p.centroid.len() < 1e-12);
}
