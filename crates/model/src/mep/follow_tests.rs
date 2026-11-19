use super::follow::{move_end, take_follow_problems};
use super::*;

fn close(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    (m, l)
}

fn duct(s: i64, l: i64, pts: Vec<(f64, f64, f64)>) -> RunSpec {
    RunSpec::new(s, l, Section::new(Domain::Duct, Shape::Round { d: 0.1 }), pts)
}

fn pipe(s: i64, l: i64, pts: Vec<(f64, f64, f64)>) -> RunSpec {
    RunSpec::pipe(s, l, PipeMaterial::Pvc, 100, pts).unwrap()
}

fn axis_legs(p: &[(f64, f64, f64)]) -> bool {
    p.windows(2).all(|w| {
        let d = [w[1].0 - w[0].0, w[1].1 - w[0].1, w[1].2 - w[0].2];
        d.iter().filter(|c| c.abs() > 1e-6).count() == 1
    })
}

fn reason(e: &Error) -> String {
    e.to_string()
}

fn refusal(key: &str) -> String {
    Error::Invalid(key.into()).to_string()
}

#[test]
fn moving_an_end_sideways_inserts_elbows_instead_of_a_diagonal() {
    // up from the end, then along y; the end slides along x
    let p = [v3(5.0, 0.0, 3.0), v3(5.0, 0.0, 4.0), v3(5.0, 6.0, 4.0)];
    let out = move_end(&p, v3(6.0, 0.0, 3.0), true).unwrap();
    let t: Vec<_> = out.iter().map(|q| (q.x, q.y, q.z)).collect();
    assert!(axis_legs(&t), "{t:?}");
    assert_eq!(t.first(), Some(&(6.0, 0.0, 3.0)));
    assert_eq!(t.last(), Some(&(5.0, 6.0, 4.0)), "the far end stays");
    assert_eq!(t.len(), 4, "{t:?}");
    // without the option the old behaviour: the corner just shifts
    let plain = move_end(&p, v3(6.0, 0.0, 3.0), false).unwrap();
    assert_eq!(plain.len(), 3);
}

#[test]
fn a_diagonal_shift_keeps_its_part_along_the_next_leg_without_folding_back() {
    let p = [v3(5.0, 0.0, 3.0), v3(5.0, 0.0, 4.0), v3(5.0, 6.0, 4.0)];
    let out = move_end(&p, v3(6.0, 1.0, 3.0), true).unwrap();
    let t: Vec<_> = out.iter().map(|q| (q.x, q.y, q.z)).collect();
    assert_eq!(t, vec![(6.0, 1.0, 3.0), (6.0, 1.0, 4.0), (5.0, 1.0, 4.0), (5.0, 6.0, 4.0)]);
}

#[test]
fn a_shift_along_the_next_leg_needs_no_elbow_and_overshoot_is_refused() {
    let p = [v3(5.0, 0.0, 3.0), v3(5.0, 0.0, 4.0), v3(5.0, 6.0, 4.0)];
    let out = move_end(&p, v3(5.0, 2.0, 3.0), true).unwrap();
    assert_eq!(out.len(), 3);
    assert!(close(out[1].y, 2.0, 1e-9));
    let e = move_end(&p, v3(5.0, 7.0, 3.0), true).unwrap_err();
    assert_eq!(reason(&e), refusal("mep.err.follow_blocked"));
}

#[test]
fn a_diagonal_stub_keeps_its_old_corner_behaviour() {
    let p = [v3(0.0, 0.0, 3.0), v3(1.0, 1.0, 3.0), v3(1.0, 5.0, 3.0)];
    let out = move_end(&p, v3(0.0, 0.0, 3.5), true).unwrap();
    assert_eq!(out.len(), 3);
}

#[test]
fn branch_with_a_corner_keeps_orthogonal_legs_when_the_main_slides() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let main = m
        .add_mep_run(&duct(sa, l, vec![(0.0, 0.0, 3.0), (8.0, 0.0, 3.0)]))
        .unwrap();
    let br = m
        .add_mep_run(&duct(sa, l, vec![(4.0, 0.0, 3.0), (4.0, 0.0, 4.0), (4.0, 5.0, 4.0)]))
        .unwrap();
    let mut s = m.mep_run(main).unwrap();
    s.points.iter_mut().for_each(|p| p.0 += 1.0);
    m.mep_edit_run(main, &s).unwrap();
    let b = m.mep_run(br).unwrap().points;
    assert!(axis_legs(&b), "{b:?}");
    assert_eq!(b[0], (5.0, 0.0, 3.0));
    assert_eq!(*b.last().unwrap(), (4.0, 5.0, 4.0));
    m.undo().unwrap();
    assert_eq!(m.mep_run(br).unwrap().points.len(), 3);
}

/// Sanitary branch falling 2 % from x=0 to the main at x=10; returns (model, main, branch).
fn drain(stack_at_start: bool) -> (Model, i64, i64) {
    let (mut m, l) = model();
    let san = m.add_mep_system("S", SystemKind::Sanitary).unwrap();
    let mut br = pipe(san, l, vec![(0.0, 0.0, 3.0), (10.0, 0.0, 3.0)]);
    br.slope = 2.0;
    let br = m.add_mep_run(&br).unwrap();
    let end = m.mep_run(br).unwrap().points[1];
    assert!(close(end.2, 2.8, 1e-9));
    let main = m
        .add_mep_run(&pipe(san, l, vec![(10.0, -5.0, 2.8), (10.0, 5.0, 2.8)]))
        .unwrap();
    if stack_at_start {
        m.add_mep_run(&pipe(san, l, vec![(0.0, 0.0, 3.0), (0.0, 0.0, 4.5)]))
            .unwrap();
    }
    (m, main, br)
}

fn lift(m: &mut Model, main: i64, dz: f64) -> Result<(), Error> {
    let mut s = m.mep_run(main).unwrap();
    s.points.iter_mut().for_each(|p| p.2 += dz);
    m.mep_edit_run(main, &s)
}

#[test]
fn a_run_with_a_fall_follows_and_keeps_its_slope_by_moving_the_free_end() {
    let (mut m, main, br) = drain(false);
    lift(&mut m, main, 0.05).unwrap();
    let b = m.mep_run(br).unwrap();
    assert!(close(b.slope, 2.0, 1e-9), "{}", b.slope);
    assert!(close(b.points[1].2, 2.85, 1e-6), "{:?}", b.points);
    assert!(close(b.points[0].2, 3.05, 1e-6), "the free end takes the change: {:?}", b.points);
    m.undo().unwrap();
    let b = m.mep_run(br).unwrap();
    assert!(close(b.points[0].2, 3.0, 1e-9) && close(b.points[1].2, 2.8, 1e-9));
}

#[test]
fn a_held_far_end_refits_the_fall_and_refuses_below_the_least() {
    let (mut m, main, br) = drain(true);
    lift(&mut m, main, 0.05).unwrap();
    let b = m.mep_run(br).unwrap();
    assert!(close(b.points[0].2, 3.0, 1e-9), "the held end stays");
    assert!(close(b.points[1].2, 2.85, 1e-6));
    assert!(close(b.slope, 1.5, 1e-6), "{}", b.slope);
    m.undo().unwrap();
    let before = m.mep_run(br).unwrap();
    // 0.15 up leaves 0.5 %: below the 1 % least of a drain
    let e = lift(&mut m, main, 0.15).unwrap_err();
    assert_eq!(reason(&e), refusal("mep.err.follow_slope"));
    assert_eq!(m.mep_run(br).unwrap(), before, "nothing is left torn");
    // lowering steepens the fall and is fine
    lift(&mut m, main, -0.1).unwrap();
    assert!(close(m.mep_run(br).unwrap().slope, 3.0, 1e-6));
}

#[test]
fn ghosts_equal_the_committed_result_and_write_nothing() {
    let (mut m, main, br) = drain(true);
    let before = m.mep_run(br).unwrap();
    let mut s = m.mep_run(main).unwrap();
    s.points.iter_mut().for_each(|p| p.2 += 0.05);
    let g = m.mep_follow_ghosts_run(main, &s).unwrap();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].run, br);
    assert_eq!(m.mep_run(br).unwrap(), before, "a preview writes nothing");
    m.mep_edit_run(main, &s).unwrap();
    let after = m.mep_run(br).unwrap().points;
    assert_eq!(g[0].points.len(), after.len());
    for (a, b) in g[0].points.iter().zip(&after) {
        assert!(close(a.0, b.0, 1e-9) && close(a.1, b.1, 1e-9) && close(a.2, b.2, 1e-9));
    }
    // the refusal shows in the preview too
    let mut s2 = m.mep_run(main).unwrap();
    s2.points.iter_mut().for_each(|p| p.2 += 0.2);
    assert!(m.mep_follow_ghosts_run(main, &s2).is_err());
}

#[test]
fn a_clean_follow_reports_no_problems_and_reports_are_taken_once() {
    let (mut m, main, _) = drain(true);
    take_follow_problems();
    lift(&mut m, main, -0.05).unwrap();
    let first = take_follow_problems();
    assert!(
        first.iter().all(|(_, i)| i.key() != "mep.issue.slope_reverse"),
        "{first:?}"
    );
    assert!(take_follow_problems().is_empty());
}
