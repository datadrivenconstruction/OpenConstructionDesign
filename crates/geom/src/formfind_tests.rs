// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::f64::consts::PI;

use super::*;

/// Square net of `n` x `n` unit cells in the xy plane, boundary supported.
fn square(n: usize) -> (Vec<V3>, Vec<(u32, u32)>, Vec<bool>) {
    let id = |i: usize, j: usize| (j * (n + 1) + i) as u32;
    let mut pos = vec![];
    let mut fixed = vec![];
    for j in 0..=n {
        for i in 0..=n {
            pos.push(v3(i as f64, j as f64, 0.0));
            fixed.push(i == 0 || j == 0 || i == n || j == n);
        }
    }
    let mut e = vec![];
    for j in 0..=n {
        for i in 0..n {
            e.push((id(i, j), id(i + 1, j)));
            e.push((id(j, i), id(j, i + 1)));
        }
    }
    (pos, e, fixed)
}

#[test]
fn string_hangs_in_the_exact_parabola() {
    let n = 12;
    let pos: Vec<V3> = (0..=n).map(|i| v3(i as f64, 0.0, 0.0)).collect();
    let e: Vec<(u32, u32)> = (0..n as u32).map(|i| (i, i + 1)).collect();
    let fixed: Vec<bool> = (0..=n).map(|i| i == 0 || i == n).collect();
    let (q, p) = (2.0, -1.0);
    let load: Vec<V3> = (0..=n).map(|_| v3(0.0, 0.0, p)).collect();
    let r = force_density(&pos, &e, &vec![q; n], &fixed, &load).unwrap();
    for (i, x) in r.pos.iter().enumerate() {
        let z = p / (2.0 * q) * (i * (n - i)) as f64;
        assert!((x.z - z).abs() < 1e-12, "node {i}: {} vs {z}", x.z);
        assert!((x.x - i as f64).abs() < 1e-12 && x.y.abs() < 1e-12);
    }
    assert!(r.residual < 1e-12);
}

#[test]
fn square_net_matches_the_analytical_solution_for_uniform_q() {
    let n = 8;
    let (pos, e, fixed) = square(n);
    let (q, p) = (3.0, -2.0);
    let load = vec![v3(0.0, 0.0, p); pos.len()];
    let r = force_density(&pos, &e, &vec![q; e.len()], &fixed, &load).unwrap();
    // discrete sine series: q lambda_kl c_kl = b_kl, lambda = 4 - 2 cos(k pi/n) - 2 cos(l pi/n)
    let nf = n as f64;
    let s = |k: usize, i: usize| (PI * (k * i) as f64 / nf).sin();
    let mut c = vec![vec![0.0; n]; n];
    for k in 1..n {
        for l in 1..n {
            let mut b = 0.0;
            for i in 1..n {
                for j in 1..n {
                    b += p * s(k, i) * s(l, j);
                }
            }
            b *= 4.0 / (nf * nf);
            let lam = 4.0 - 2.0 * (PI * k as f64 / nf).cos() - 2.0 * (PI * l as f64 / nf).cos();
            c[k][l] = b / (q * lam);
        }
    }
    for j in 0..=n {
        for i in 0..=n {
            let mut z = 0.0;
            for k in 1..n {
                for l in 1..n {
                    z += c[k][l] * s(k, i) * s(l, j);
                }
            }
            let x = r.pos[j * (n + 1) + i];
            assert!((x.z - z).abs() < 1e-11, "({i},{j}): {} vs {z}", x.z);
            assert!((x.x - i as f64).abs() < 1e-11 && (x.y - j as f64).abs() < 1e-11);
        }
    }
    // the supports carry the whole load: vertical edge forces into the boundary sum to it
    let free = fixed.iter().filter(|f| !**f).count() as f64;
    let mut up = 0.0;
    for &(a, b) in &e {
        let (fa, fb) = (fixed[a as usize], fixed[b as usize]);
        if fa != fb {
            let (s, f) = if fa { (a, b) } else { (b, a) };
            up += q * (r.pos[f as usize].z - r.pos[s as usize].z);
        }
    }
    assert!((up - p * free).abs() < 1e-10, "{up} vs {}", p * free);
}

#[test]
fn numbering_does_not_change_the_solution() {
    let n = 6;
    let (pos, e, fixed) = square(n);
    let load = vec![v3(0.1, 0.0, -1.0); pos.len()];
    let q: Vec<f64> = (0..e.len()).map(|k| 1.0 + (k % 3) as f64 * 0.5).collect();
    let a = force_density(&pos, &e, &q, &fixed, &load).unwrap();
    // reverse the node numbers
    let m = pos.len() as u32;
    let rev = |i: u32| m - 1 - i;
    let pos2: Vec<V3> = pos.iter().rev().copied().collect();
    let fixed2: Vec<bool> = fixed.iter().rev().copied().collect();
    let e2: Vec<(u32, u32)> = e.iter().map(|&(x, y)| (rev(y), rev(x))).collect();
    let b = force_density(&pos2, &e2, &q, &fixed2, &load).unwrap();
    for i in 0..pos.len() {
        assert!((a.pos[i] - b.pos[rev(i as u32) as usize]).len() < 1e-12);
    }
    for k in 0..e.len() {
        assert!((a.force[k] - b.force[k]).abs() < 1e-12);
    }
}

#[test]
fn unsupported_and_bad_inputs_are_named() {
    let (pos, mut e, fixed) = square(2);
    let mut pos = pos;
    pos.push(v3(5.0, 5.0, 0.0));
    let mut fixed = fixed;
    fixed.push(false);
    pos.push(v3(6.0, 5.0, 0.0));
    fixed.push(false);
    e.push((9, 10));
    let load = vec![v3(0.0, 0.0, -1.0); pos.len()];
    let q = vec![1.0; e.len()];
    match force_density(&pos, &e, &q, &fixed, &load) {
        Err(FormFindError::Singular(m)) => assert!(m.contains("no path to a support"), "{m}"),
        other => panic!("{other:?}"),
    }
    let mut bad = q.clone();
    bad[3] = -1.0;
    assert!(matches!(
        force_density(&pos, &e, &bad, &fixed, &load),
        Err(FormFindError::Input(m)) if m.contains("edge 3")
    ));
}

#[test]
fn relaxation_of_density_edges_equals_the_linear_solution() {
    let n = 5;
    let (pos, e, fixed) = square(n);
    let load = vec![v3(0.0, 0.0, -1.0); pos.len()];
    let q = vec![2.0; e.len()];
    let lin = force_density(&pos, &e, &q, &fixed, &load).unwrap();
    let law: Vec<EdgeLaw> = q.iter().map(|&v| EdgeLaw::Density(v)).collect();
    let r = relax(
        &pos,
        &Relax {
            edges: &e,
            law: &law,
            bends: &[],
            fixed: &fixed,
            load: &load,
            tol: 1e-10,
            max_steps: 200_000,
        },
    )
    .unwrap();
    for (a, b) in r.pos.iter().zip(&lin.pos) {
        assert!((*a - *b).len() < 1e-8, "{a:?} vs {b:?}");
    }
}

#[test]
fn bending_active_lath_buckles_into_the_elastica() {
    // a straight lath pushed 1 % shorter between pins rises into the first buckling mode
    let (n, l, short) = (20usize, 10.0, 0.1);
    let span = l - short;
    let pos: Vec<V3> = (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            v3(span * t, 0.0, 0.05 * (PI * t).sin())
        })
        .collect();
    let e: Vec<(u32, u32)> = (0..n as u32).map(|i| (i, i + 1)).collect();
    let law = vec![
        EdgeLaw::Elastic {
            ea: 1e4,
            l0: l / n as f64
        };
        n
    ];
    let bends: Vec<Bend> = (1..n as u32)
        .map(|b| Bend {
            a: b - 1,
            b,
            c: b + 1,
            ei: 10.0,
        })
        .collect();
    let fixed: Vec<bool> = (0..=n).map(|i| i == 0 || i == n).collect();
    let load = vec![v3(0.0, 0.0, 0.0); n + 1];
    let r = relax(
        &pos,
        &Relax {
            edges: &e,
            law: &law,
            bends: &bends,
            fixed: &fixed,
            load: &load,
            tol: 1e-9,
            max_steps: 400_000,
        },
    )
    .unwrap();
    assert!(r.residual < 1e-9);
    let arc: f64 = e.iter().map(|&(a, b)| (r.pos[b as usize] - r.pos[a as usize]).len()).sum();
    // inextensible to the axial strain N / EA
    assert!((arc - l).abs() < 1e-3, "arc {arc}");
    let rise = r.pos.iter().map(|p| p.z).fold(f64::MIN, f64::max);
    let expect = 2.0 / PI * (arc * (arc - span)).sqrt();
    assert!((rise - expect).abs() < 0.02 * expect, "rise {rise} vs {expect}");
    for i in 0..=n {
        assert!((r.pos[i].z - r.pos[n - i].z).abs() < 1e-5, "symmetry at {i}");
        assert!(r.pos[i].y.abs() < 1e-12);
    }
    // compression close to the Euler load pi^2 EI / L^2
    let euler = PI * PI * 10.0 / (l * l);
    let mid = -r.force[n / 2];
    assert!((mid - euler).abs() < 0.03 * euler, "N {mid} vs {euler}");
}
