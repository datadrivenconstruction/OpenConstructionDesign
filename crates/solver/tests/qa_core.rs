// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! QA: public API of ddc-solver, one case per item plus edge cases and invariants.
use ddc_solver::*;
use std::f64::consts::{PI, TAU};

fn c(tag: i64, rule: Rule) -> Constraint {
    Constraint { tag, rule }
}

/// Deterministic LCG, fixed seed.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

#[test]
fn pt_reads_pair() {
    let x = [1.0, 2.0, 3.0, 4.0];
    assert_eq!(pt(&x, 0), (1.0, 2.0));
    assert_eq!(pt(&x, 1), (3.0, 4.0));
}

#[test]
#[should_panic]
fn pt_out_of_range_panics() {
    let _ = pt(&[0.0, 0.0], 1);
}

#[test]
fn dir_is_b_minus_a() {
    let x = [1.0, 1.0, 4.0, 5.0];
    assert_eq!(dir(&x, (0, 1)), (3.0, 4.0));
    assert_eq!(dir(&x, (1, 0)), (-3.0, -4.0));
    assert_eq!(dir(&x, (0, 0)), (0.0, 0.0));
}

#[test]
fn unit_is_unit_and_degenerate_is_finite() {
    let x = [0.0, 0.0, 3.0, 4.0];
    let u = unit(&x, (0, 1));
    assert!((u.0 - 0.6).abs() < 1e-15 && (u.1 - 0.8).abs() < 1e-15);
    assert_eq!(unit(&x, (0, 0)), (0.0, 0.0));
    let huge = [0.0, 0.0, 1e300, 1e300];
    let u = unit(&huge, (0, 1));
    assert!(u.0.is_finite() && u.1.is_finite());
}

#[test]
fn wrap_angle_range_and_periodicity() {
    assert_eq!(wrap_angle(0.0), 0.0);
    assert!((wrap_angle(PI) - PI).abs() < 1e-12);
    assert!((wrap_angle(-PI) - PI).abs() < 1e-12, "range is half-open at -pi");
    let mut r = Lcg(7);
    for _ in 0..2000 {
        let a = (r.next() - 0.5) * 200.0;
        let w = wrap_angle(a);
        assert!(w > -PI - 1e-12 && w <= PI + 1e-12, "{a} -> {w}");
        let k = ((a - w) / TAU).round();
        assert!((a - w - k * TAU).abs() < 1e-9);
        assert!((wrap_angle(w) - w).abs() < 1e-12, "idempotent");
    }
    assert!(wrap_angle(f64::NAN).is_nan());
}

#[test]
fn sdist_sign_left_positive() {
    let x = [0.0, 0.0, 10.0, 0.0, 5.0, 2.0, 5.0, -3.0];
    assert!((sdist(&x, (0, 1), 2) - 2.0).abs() < 1e-12);
    assert!((sdist(&x, (0, 1), 3) + 3.0).abs() < 1e-12);
    assert!((sdist(&x, (1, 0), 2) + 2.0).abs() < 1e-12, "reverse flips sign");
    assert!(sdist(&x, (0, 0), 2).is_finite(), "degenerate line");
}

#[test]
fn gauss_solves_known_system() {
    let y = gauss(vec![2.0, 1.0, 1.0, 3.0], vec![5.0, 10.0], 2);
    assert!((y[0] - 1.0).abs() < 1e-12 && (y[1] - 3.0).abs() < 1e-12);
}

#[test]
fn gauss_pivots_zero_diagonal() {
    let y = gauss(vec![0.0, 1.0, 1.0, 0.0], vec![2.0, 3.0], 2);
    assert!((y[0] - 3.0).abs() < 1e-12 && (y[1] - 2.0).abs() < 1e-12);
}

#[test]
fn gauss_singular_and_empty() {
    let y = gauss(vec![0.0, 0.0, 0.0, 0.0], vec![1.0, 1.0], 2);
    assert_eq!(y, vec![0.0, 0.0]);
    assert!(gauss(vec![], vec![], 0).is_empty());
    let y = gauss(vec![f64::NAN; 4], vec![1.0, 1.0], 2);
    assert_eq!(y.len(), 2, "NaN input must not panic");
}

#[test]
fn gauss_random_spd_residual() {
    let mut r = Lcg(42);
    for m in 1..8 {
        let mut a = vec![0.0; m * m];
        for v in a.iter_mut() {
            *v = r.next() - 0.5;
        }
        let mut s = vec![0.0; m * m];
        for i in 0..m {
            for j in 0..m {
                s[i * m + j] = (0..m).map(|k| a[k * m + i] * a[k * m + j]).sum::<f64>()
                    + if i == j { 1.0 } else { 0.0 };
            }
        }
        let b: Vec<f64> = (0..m).map(|_| r.next()).collect();
        let y = gauss(s.clone(), b.clone(), m);
        for i in 0..m {
            let lhs: f64 = (0..m).map(|j| s[i * m + j] * y[j]).sum();
            assert!((lhs - b[i]).abs() < 1e-9);
        }
    }
}

#[test]
fn solve_dist_moves_free_point_only() {
    let x0 = [0.0, 0.0, 3.0, 0.0];
    let cs = [c(1, Rule::Dist(0, 1, 5.0))];
    let r = solve(&cs, &x0, &[0]).unwrap();
    assert_eq!((r[0], r[1]), (0.0, 0.0), "pinned point stays");
    assert!((r[2].hypot(r[3]) - 5.0).abs() < 1e-6);
}

#[test]
fn solve_is_deterministic() {
    let x0 = [0.0, 0.0, 3.0, 1.0, 5.0, 4.0, -1.0, 6.0];
    let cs = [
        c(1, Rule::Dist(0, 1, 4.0)),
        c(2, Rule::Perp((0, 1), (1, 2))),
        c(3, Rule::Parallel((0, 1), (3, 2))),
    ];
    let a = solve(&cs, &x0, &[0]).unwrap();
    let b = solve(&cs, &x0, &[0]).unwrap();
    assert_eq!(a, b);
}

#[test]
fn solve_empty_and_unconstrained() {
    assert_eq!(solve(&[], &[], &[]).unwrap(), Vec::<f64>::new());
    let x0 = [1.0, 2.0, 3.0, 4.0];
    assert_eq!(solve(&[], &x0, &[]).unwrap(), x0.to_vec());
}

#[test]
fn solve_fix_rule_and_out_of_range_pin() {
    let x0 = [0.0, 0.0, 3.0, 0.0];
    let cs = [c(1, Rule::Fix(0)), c(2, Rule::Dist(0, 1, 2.0))];
    let r = solve(&cs, &x0, &[99]).unwrap();
    assert_eq!((r[0], r[1]), (0.0, 0.0));
    assert!((r[2].hypot(r[3]) - 2.0).abs() < 1e-6);
}

#[test]
fn solve_conflict_reports_tags() {
    let x0 = [0.0, 0.0, 3.0, 0.0];
    let cs = [c(10, Rule::Dist(0, 1, 5.0)), c(11, Rule::Dist(0, 1, 7.0))];
    let e = solve(&cs, &x0, &[0]).unwrap_err();
    assert!(!e.tags.is_empty());
    assert!(e.tags.iter().all(|(t, r)| (*t == 10 || *t == 11) && *r > 0.0));
    let s = e.to_string();
    assert!(s.starts_with("constraints conflict:") && s.contains('#'));
}

#[test]
fn solve_rules_each_satisfied() {
    let x0 = [0.0, 0.0, 10.0, 0.5, 10.5, 9.0, -0.5, 10.0];
    let cs = [
        c(1, Rule::Parallel((0, 1), (3, 2))),
        c(2, Rule::Perp((0, 1), (1, 2))),
        c(3, Rule::LineDist((0, 1), (3, 2), 10.0)),
    ];
    let r = solve(&cs, &x0, &[0, 1]).unwrap();
    let (u, v) = (unit(&r, (0, 1)), unit(&r, (3, 2)));
    assert!((u.0 * v.1 - u.1 * v.0).abs() < 1e-6);
    let w = unit(&r, (1, 2));
    assert!((u.0 * w.0 + u.1 * w.1).abs() < 1e-6);
    assert!((sdist(&r, (0, 1), 3) - 10.0).abs() < 1e-5);
    let cs2 = [c(1, Rule::Collinear((0, 1), (2, 3)))];
    let r2 = solve(&cs2, &x0, &[0, 1]).unwrap();
    assert!(sdist(&r2, (0, 1), 2).abs() < 1e-6 && sdist(&r2, (0, 1), 3).abs() < 1e-6);
}

#[test]
fn solve_equal_chain_gives_even_spacing() {
    let x0 = [
        0.0, 0.0, 10.0, 0.0, 0.0, 3.0, 10.0, 3.0, 0.0, 10.0, 10.0, 10.0,
    ];
    let cs = [c(1, Rule::Equal(vec![(0, 1), (2, 3), (4, 5)]))];
    let r = solve(&cs, &x0, &[0, 1]).unwrap();
    let g0 = sdist(&r, (0, 1), 2);
    let g1 = sdist(&r, (2, 3), 4);
    assert!((g0 - g1).abs() < 1e-5, "{g0} vs {g1}");
}

#[test]
fn solve_nan_does_not_panic() {
    let x0 = [0.0, 0.0, f64::NAN, 0.0];
    let cs = [c(1, Rule::Dist(0, 1, 5.0))];
    let _ = solve(&cs, &x0, &[0]);
}

#[test]
fn solve_huge_coordinates_finite() {
    let x0 = [0.0, 0.0, 1e9, 0.0];
    let cs = [c(1, Rule::Dist(0, 1, 2e9))];
    if let Ok(r) = solve(&cs, &x0, &[0]) {
        assert!(r.iter().all(|v| v.is_finite()));
    }
}

#[test]
fn solve_random_distance_chains() {
    let mut rng = Lcg(2024);
    for _ in 0..50 {
        let n = 4;
        let x0: Vec<f64> = (0..2 * n).map(|_| rng.next() * 10.0).collect();
        let t: Vec<f64> = x0.iter().map(|v| v + (rng.next() - 0.5)).collect();
        let cs: Vec<Constraint> = (0..n - 1)
            .map(|i| {
                let d = (t[2 * i + 2] - t[2 * i]).hypot(t[2 * i + 3] - t[2 * i + 1]);
                c(i as i64, Rule::Dist(i, i + 1, d))
            })
            .collect();
        let r = solve(&cs, &x0, &[0]).expect("consistent chain solvable");
        for (i, k) in cs.iter().enumerate() {
            if let Rule::Dist(a, b, d) = k.rule {
                let (p, q) = (pt(&r, a), pt(&r, b));
                assert!(((q.0 - p.0).hypot(q.1 - p.1) - d).abs() < 1e-5, "chain {i}");
            }
        }
    }
}

#[test]
fn row_space_rank_and_dependence() {
    let mut s = RowSpace::default();
    assert_eq!(s.rank(), 0);
    assert!(!RowSpace::add(&mut s, &[0.0, 0.0]), "zero row rejected");
    assert!(!RowSpace::add(&mut s, &[]), "empty row rejected");
    assert!(RowSpace::add(&mut s, &[1.0, 0.0, 0.0]));
    assert!(RowSpace::add(&mut s, &[0.0, 2.0, 0.0]));
    assert!(!RowSpace::add(&mut s, &[3.0, 5.0, 0.0]), "dependent row");
    assert!(RowSpace::add(&mut s, &[1.0, 1.0, 1.0]));
    assert_eq!(s.rank(), 3);
    assert!(!RowSpace::add(&mut s, &[7.0, -2.0, 9.0]), "full rank: everything dependent");
}

#[test]
fn row_space_coverage() {
    let mut s = RowSpace::default();
    assert_eq!(s.coverage(0), 0.0);
    RowSpace::add(&mut s, &[1.0, 1.0]);
    assert!((s.coverage(0) - 0.5).abs() < 1e-12);
    assert!((s.coverage(1) - 0.5).abs() < 1e-12);
    assert_eq!(s.coverage(5), 0.0, "out of range column");
    RowSpace::add(&mut s, &[1.0, -1.0]);
    assert!((s.coverage(0) - 1.0).abs() < 1e-12);
}

#[test]
fn min_conflict_set_cases() {
    assert!(min_conflict_set(0, |_| false).is_empty());
    assert!(min_conflict_set(5, |_| true).is_empty());
    let bad = |s: &[usize]| !(s.contains(&1) && s.contains(&3));
    let r = min_conflict_set(6, bad);
    let mut sorted = r.clone();
    sorted.sort();
    assert_eq!(sorted, vec![1, 3]);
    assert_eq!(r[0], 3, "culprit first");
    assert_eq!(min_conflict_set(4, |s| !s.contains(&2)), vec![2]);
}

#[test]
fn constants_and_clone_eq() {
    assert!(TOL > 0.0 && TOL < 1e-3);
    let k = c(1, Rule::Equal(vec![(0, 1)]));
    assert_eq!(k.clone(), k);
}
