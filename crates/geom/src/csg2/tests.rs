// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::math::v3;
use proptest::prelude::*;
use std::f64::consts::{PI, TAU};

const TOL: Tol = Tol {
    length: 1e-9,
    angle: 1e-9,
};

fn boxm(lo: V3, hi: V3) -> Mesh {
    let p = |i: u32| {
        v3(
            if i & 1 > 0 { hi.x } else { lo.x },
            if i & 2 > 0 { hi.y } else { lo.y },
            if i & 4 > 0 { hi.z } else { lo.z },
        )
    };
    let quads = [
        [0, 2, 3, 1], // z-
        [4, 5, 7, 6], // z+
        [0, 1, 5, 4], // y-
        [2, 6, 7, 3], // y+
        [0, 4, 6, 2], // x-
        [1, 3, 7, 5], // x+
    ];
    let mut m = Mesh {
        positions: (0..8).map(p).collect(),
        ..Default::default()
    };
    for (f, q) in quads.iter().enumerate() {
        m.triangles.push([q[0], q[1], q[2]]);
        m.triangles.push([q[0], q[2], q[3]]);
        m.face_of.extend([f as u32; 2]);
    }
    m
}

/// UV sphere with single-vertex poles: `2 * nu * (nv - 1)` triangles.
fn sphere(c: V3, r: f64, nu: usize, nv: usize) -> Mesh {
    let mut m = Mesh::default();
    m.positions.push(c + v3(0., 0., -r));
    for j in 1..nv {
        let th = PI * j as f64 / nv as f64 - PI / 2.0;
        for i in 0..nu {
            let ph = TAU * i as f64 / nu as f64;
            m.positions
                .push(c + v3(th.cos() * ph.cos(), th.cos() * ph.sin(), th.sin()) * r);
        }
    }
    m.positions.push(c + v3(0., 0., r));
    let top = (m.positions.len() - 1) as u32;
    let ring = |j: usize, i: usize| (1 + (j - 1) * nu + i % nu) as u32;
    for i in 0..nu {
        m.triangles.push([0, ring(1, i + 1), ring(1, i)]);
        m.triangles
            .push([top, ring(nv - 1, i), ring(nv - 1, i + 1)]);
    }
    for j in 1..nv - 1 {
        for i in 0..nu {
            let (a, b, c2, d) = (
                ring(j, i),
                ring(j, i + 1),
                ring(j + 1, i + 1),
                ring(j + 1, i),
            );
            m.triangles.push([a, b, c2]);
            m.triangles.push([a, c2, d]);
        }
    }
    m.face_of = vec![7; m.triangles.len()];
    m
}

/// Vertical n-gon prism around (cx, cy), caps fanned from their centres: `4 n` triangles.
fn cyl(cx: f64, cy: f64, r: f64, z0: f64, z1: f64, n: usize) -> Mesh {
    let mut m = Mesh::default();
    for z in [z0, z1] {
        for i in 0..n {
            let a = TAU * i as f64 / n as f64;
            m.positions.push(v3(cx + r * a.cos(), cy + r * a.sin(), z));
        }
    }
    let (c0, c1) = (2 * n as u32, 2 * n as u32 + 1);
    m.positions.push(v3(cx, cy, z0));
    m.positions.push(v3(cx, cy, z1));
    let n32 = n as u32;
    for i in 0..n32 {
        let j = (i + 1) % n32;
        m.triangles.push([i, j, j + n32]);
        m.triangles.push([i, j + n32, i + n32]);
        m.triangles.push([c0, j, i]);
        m.triangles.push([c1, i + n32, j + n32]);
    }
    m.face_of = vec![9; m.triangles.len()];
    m
}

fn rotated(m: &Mesh, ax: f64, az: f64, d: V3) -> Mesh {
    let (sx, cx, sz, cz) = (ax.sin(), ax.cos(), az.sin(), az.cos());
    let mut out = m.clone();
    for p in out.positions.iter_mut() {
        let q = v3(p.x, cx * p.y - sx * p.z, sx * p.y + cx * p.z);
        *p = v3(cz * q.x - sz * q.y, sz * q.x + cz * q.y, q.z) + d;
    }
    out
}

fn run(a: &Mesh, b: &Mesh, op: BoolOp) -> Mesh {
    let r = mesh_boolean(a, b, op, TOL).unwrap_or_else(|e| panic!("{op:?}: {e}"));
    assert!(r.mesh.is_watertight(), "{op:?} not closed");
    assert_eq!(r.mesh.triangles.len(), r.mesh.face_of.len());
    r.mesh
}

/// Union/intersection/difference volume identities hold to `eps` and every result is closed.
fn identities(a: &Mesh, b: &Mesh, eps: f64) -> (f64, f64, f64) {
    let (va, vb) = (a.volume(), b.volume());
    let u = run(a, b, BoolOp::Union).volume();
    let i = run(a, b, BoolOp::Intersect).volume();
    let s = run(a, b, BoolOp::Subtract).volume();
    let t = run(b, a, BoolOp::Subtract).volume();
    assert!(
        (u + i - va - vb).abs() < eps,
        "u {u} + i {i} != {va} + {vb}"
    );
    assert!((s + i - va).abs() < eps, "s {s} + i {i} != {va}");
    assert!((t + i - vb).abs() < eps, "t {t} + i {i} != {vb}");
    assert!(
        (u - s - t - i).abs() < eps,
        "u {u} != s {s} + t {t} + i {i}"
    );
    (u, i, s)
}

#[test]
fn boxes_with_shared_planes_give_exact_volumes() {
    let a = boxm(v3(0., 0., 0.), v3(2., 2., 2.));
    let b = boxm(v3(1., 0., 0.), v3(3., 2., 2.));
    let (u, i, s) = identities(&a, &b, 1e-12);
    assert!((u - 12.0).abs() < 1e-12 && (i - 4.0).abs() < 1e-12 && (s - 4.0).abs() < 1e-12);
    let r = mesh_boolean(&a, &b, BoolOp::Union, TOL).unwrap();
    assert!(r.stats.coplanar_pairs > 0, "{:?}", r.stats);
}

#[test]
fn face_to_face_contact_merges_or_vanishes() {
    let a = boxm(v3(0., 0., 0.), v3(1., 1., 1.));
    let b = boxm(v3(1., 0., 0.), v3(2., 1., 1.));
    assert!((run(&a, &b, BoolOp::Union).volume() - 2.0).abs() < 1e-12);
    let i = run(&a, &b, BoolOp::Intersect);
    assert!(i.volume().abs() < 1e-12, "{}", i.volume());
    assert!((run(&a, &b, BoolOp::Subtract).volume() - 1.0).abs() < 1e-12);
    // partial contact, the touching rectangle lies inside one face
    let c = boxm(v3(1., 0.25, 0.5), v3(1.5, 0.75, 0.75));
    let u = run(&a, &c, BoolOp::Union);
    assert!((u.volume() - 1.0 - 0.0625).abs() < 1e-12, "{}", u.volume());
    // the merged union has no internal faces: area = both areas minus twice the contact
    let area = 6.0 + 2.0 * (0.5 * 0.25 + 0.5 * 0.25 + 0.5 * 0.5) - 2.0 * 0.125;
    assert!((u.area() - area).abs() < 1e-12, "{} vs {area}", u.area());
}

#[test]
fn coplanar_faces_with_crossing_diagonals() {
    // same top plane, rotated footprint: every top triangle pair is coplanar and the
    // diagonals cross in the plane
    let a = boxm(v3(0., 0., 0.), v3(2., 2., 1.));
    let b = rotated(
        &boxm(v3(-0.7, -0.7, -1.), v3(0.7, 0.7, 1.)),
        0.0,
        0.3,
        v3(1., 1., 0.),
    );
    let (u, i, _) = identities(&a, &b, 1e-12);
    assert!((i - 1.96).abs() < 1e-12, "{i}");
    assert!((u - (4.0 + 3.92 - 1.96)).abs() < 1e-12, "{u}");
}

#[test]
fn near_tangent_contacts_stay_closed() {
    let slab = boxm(v3(-2., -2., -1.), v3(2., 2., 1.));
    // sphere on the slab: touching at the pole, 1e-9 into it, 1e-9 above it
    for d in [0.0, 1e-9, -1e-9] {
        let s = sphere(v3(0., 0., 2.0 - d), 1.0, 32, 17);
        identities(&slab, &s, 1e-9);
    }
    // rotated sphere whose lowest-x vertex enters the slab side by 1e-9: nearly tangent facets
    let s = rotated(
        &sphere(v3(0., 0., 0.), 1.0, 40, 21),
        0.4,
        0.2,
        v3(0., 0., 0.),
    );
    let min_x = s.positions.iter().map(|p| p.x).fold(f64::MAX, f64::min);
    let s = rotated(&s, 0.0, 0.0, v3(2.0 - 1e-9 - min_x, 0.1, 0.2));
    identities(&slab, &s, 1e-9);
    // prism inscribed in the slab: ring vertices on x = +-1 exactly, on y = +-1 within 1e-16
    let inner = boxm(v3(-1., -1., -1.), v3(1., 1., 1.));
    let c = cyl(0., 0., 1.0, -2., 2., 64);
    let (_, i, s) = identities(&inner, &c, 1e-9);
    let ring = 0.5 * 64.0 * (TAU / 64.0).sin();
    assert!((i - 2.0 * ring).abs() < 1e-9, "{i}");
    assert!((s - (8.0 - 2.0 * ring)).abs() < 1e-9, "{s}");
}

#[test]
fn many_cuts_in_one_slab() {
    let n = 24;
    let disc = 0.5 * n as f64 * 0.3f64.powi(2) * (TAU / n as f64).sin();
    let mut m = boxm(v3(0., 0., 0.), v3(8., 8., 0.3));
    for i in 0..8 {
        for j in 0..8 {
            let c = cyl(0.5 + i as f64, 0.5 + j as f64, 0.3, -0.1, 0.4, n);
            m = run(&m, &c, BoolOp::Subtract);
        }
    }
    let want = 64.0 * 0.3 - 64.0 * disc * 0.3;
    assert!((m.volume() - want).abs() < 1e-9, "{} vs {want}", m.volume());
    // a second row of holes overlapping the first ones, and a blind pocket ending mid-slab
    let mut v = m.volume();
    for i in 0..8 {
        let c = cyl(0.75 + i as f64, 0.5, 0.3, -0.1, 0.4, n);
        let i_vol = run(&m, &c, BoolOp::Intersect).volume();
        m = run(&m, &c, BoolOp::Subtract);
        assert!((m.volume() - (v - i_vol)).abs() < 1e-9);
        v = m.volume();
    }
    let pocket = cyl(4.0, 4.0, 0.45, 0.1, 0.5, 48);
    let i_vol = run(&m, &pocket, BoolOp::Intersect).volume();
    let r = run(&m, &pocket, BoolOp::Subtract);
    assert!((r.volume() - (v - i_vol)).abs() < 1e-9);
    assert!(i_vol > 0.0);
}

#[test]
fn open_and_non_finite_operands_are_refused() {
    let a = boxm(v3(0., 0., 0.), v3(1., 1., 1.));
    let mut open = a.clone();
    open.triangles.pop();
    assert_eq!(
        mesh_boolean(&a, &open, BoolOp::Union, TOL).unwrap_err(),
        MeshBoolError::Open { which: 'b' }
    );
    let mut bad = a.clone();
    bad.positions[3].x = f64::NAN;
    assert_eq!(
        mesh_boolean(&bad, &a, BoolOp::Union, TOL).unwrap_err(),
        MeshBoolError::NonFinite { which: 'a' }
    );
}

#[test]
fn self_intersecting_operand_is_refused_before_the_boolean() {
    let mut two = boxm(v3(0., 0., 0.), v3(1., 1., 1.));
    // off the face diagonals, so edges cross face interiors strictly
    let other = boxm(v3(0.5, 0.3, 0.6), v3(1.5, 1.3, 1.6));
    let n = two.positions.len() as u32;
    two.positions.extend(other.positions.iter().copied());
    two.triangles
        .extend(other.triangles.iter().map(|t| t.map(|i| i + n)));
    two.face_of.extend(other.face_of.iter().copied());
    let c = boxm(v3(3., 3., 3.), v3(4., 4., 4.));
    assert_eq!(
        mesh_boolean(&two, &c, BoolOp::Union, TOL).unwrap_err(),
        MeshBoolError::SelfIntersecting { which: 'a' }
    );
    assert_eq!(
        mesh_boolean(&c, &two, BoolOp::Subtract, TOL).unwrap_err(),
        MeshBoolError::SelfIntersecting { which: 'b' }
    );
    // two boxes touching along a face are not crossing: accepted
    let mut touch = boxm(v3(0., 0., 0.), v3(1., 1., 1.));
    let side = boxm(v3(1., 0., 0.), v3(2., 1., 1.));
    let n = touch.positions.len() as u32;
    touch.positions.extend(side.positions.iter().copied());
    touch
        .triangles
        .extend(side.triangles.iter().map(|t| t.map(|i| i + n)));
    touch.face_of.extend(side.face_of.iter().copied());
    assert!(mesh_boolean(&touch, &c, BoolOp::Union, TOL).is_ok());
    assert!(MeshBoolError::SelfIntersecting { which: 'a' }
        .fallback_note()
        .contains("intersects itself"));
}

#[test]
fn face_ids_survive() {
    let a = boxm(v3(0., 0., 0.), v3(2., 2., 2.));
    let s = sphere(v3(2., 2., 2.), 1.0, 24, 13);
    let r = run(&a, &s, BoolOp::Subtract);
    let mut ids: Vec<u32> = r.face_of.clone();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids, vec![0, 1, 2, 3, 4, 5, 7]);
}

#[test]
fn body_path_keeps_tags_and_validates() {
    let a = Body::from_mesh(&sphere(v3(0., 0., 0.), 1.0, 48, 25), 3);
    let b = crate::mass::cuboid(v3(0.013, 0.021, -2.), v3(2., 2., 2.)).retagged(10);
    let r = boolean_bodies(&a, &b, BoolOp::Subtract, Tol::default()).unwrap();
    r.validate(Tol::default()).unwrap();
    let i = boolean_bodies(&a, &b, BoolOp::Intersect, Tol::default()).unwrap();
    i.validate(Tol::default()).unwrap();
    let full = a.volume();
    assert!(
        // welding at the 1e-6 length tolerance may move vertices by that much
        (r.volume() + i.volume() - full).abs() < 1e-6,
        "{} + {} vs {}",
        r.volume(),
        i.volume(),
        full
    );
    assert!(i.volume() > 0.2 * full && i.volume() < 0.25 * full);
    assert!(r.tags().contains(&3) && r.tags().iter().any(|t| *t >= 10));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn volume_identities_hold(
        ax in 0.0f64..PI, az in 0.0f64..PI,
        dx in -1.2f64..1.2, dy in -1.2f64..1.2, dz in -1.2f64..1.2,
        r in 0.3f64..1.1,
    ) {
        let a = rotated(&boxm(v3(-1., -1., -0.6), v3(1., 1., 0.6)), ax, az, v3(0., 0., 0.));
        let b = rotated(&sphere(v3(0., 0., 0.), r, 16, 9), az, ax, v3(dx, dy, dz));
        identities(&a, &b, 1e-9);
        let c = rotated(&cyl(0., 0., r * 0.6, -1.5, 1.5, 12), ax, 0.0, v3(dx, dy, dz));
        identities(&b, &c, 1e-9);
    }
}

#[test]
fn sphere_50k_minus_cylinder_within_budget() {
    let s = sphere(v3(0., 0., 0.), 1.0, 250, 101);
    assert_eq!(s.triangles.len(), 50_000);
    let c = rotated(
        &cyl(0., 0., 0.4, -1.5, 1.5, 128),
        0.31,
        0.17,
        v3(0.05, -0.03, 0.0),
    );
    let t = std::time::Instant::now();
    let r = mesh_boolean(&s, &c, BoolOp::Subtract, TOL).unwrap();
    let secs = t.elapsed().as_secs_f64();
    // budget: 1 s optimised, 20 s in the unoptimised test profile
    let budget = if cfg!(debug_assertions) { 20.0 } else { 1.0 };
    eprintln!("sphere 50k - cylinder: {secs:.3} s, {:?}", r.stats);
    assert!(r.mesh.is_watertight());
    let i = run(&s, &c, BoolOp::Intersect).volume();
    assert!((r.mesh.volume() + i - s.volume()).abs() < 1e-9);
    // exact solid: 4/3 pi (1 - (1 - 0.4^2)^1.5) = 0.964 for a centred axis
    assert!(i > 0.85 && i < 1.1, "{i}");
    assert!(secs < budget, "{secs} s over {budget} s");
}
