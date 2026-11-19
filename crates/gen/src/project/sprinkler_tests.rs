//! Unit tests of the sprinkler coverage measure.

use super::rules::rule;

use super::sprinkler::*;

/// An L-shaped room: the head grid of its box, the heads that stand in it cover it, and
/// taking one away leaves a measured gap.
#[test]
fn coverage_is_measured_not_assumed() {
    let l: Vec<P2> = vec![(0.0, 0.0), (9.0, 0.0), (9.0, 3.0), (3.0, 3.0), (3.0, 9.0), (0.0, 9.0)];
    let (xs, ys) = grid(&bbox(&l));
    let heads: Vec<P2> =
        xs.iter().flat_map(|x| ys.iter().map(move |y| (*x, *y))).filter(|q| placeable(&l, *q)).collect();
    let reach = (rule("fire.head_area") / 2.0).sqrt() + 1e-6;
    let step = rule("fire.head_spacing") / SAMPLES as f64;
    let (full, gap) = coverage(&l, &heads, reach, step);
    assert!(full > 0.95, "{full}");
    assert!(heads.len() > 2 && (full < 1.0 || gap.is_none()));
    let fewer: Vec<P2> = heads.iter().skip(1).copied().collect();
    let (less, hole) = coverage(&l, &fewer, reach, step);
    assert!(less < full && hole.is_some(), "{less} vs {full}");
}
