// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A new riser meets the risers of its system already there: one storey's riser stands on the
//! one below (end to end, on its axis) instead of overlapping it, and a riser that already passes
//! the level is tapped instead of doubled. Without this every storey after the first, and every
//! level of a building whose generator already laid the risers, was refused as blocked.

use ddc_geom::math::{v2, v3, V2, V3};

const EPS: f64 = 1e-6;

/// A vertical piece of a run that crosses a level: a riser of its system.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Stack {
    pub system: String,
    pub at: V2,
    pub z0: f64,
    pub z1: f64,
}

/// The riser `a..b` as a stack when it is vertical and crosses one of the level elevations
/// (drops to fixtures stay inside a storey and are no risers).
pub(super) fn stack_of(system: &str, a: V3, b: V3, levels: &[f64]) -> Option<Stack> {
    let vertical = v2(a.x - b.x, a.y - b.y).len() < EPS;
    let (z0, z1) = (a.z.min(b.z), a.z.max(b.z));
    let crosses = levels.iter().any(|&z| z0 < z - EPS && z1 > z + EPS);
    (vertical && crosses).then(|| Stack {
        system: system.to_string(),
        at: v2(a.x, a.y),
        z0,
        z1,
    })
}

/// How the riser asked at a plan point meets the stacks of its system.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Join {
    /// A stack already passes the layer the branches leave at: they tee into it.
    Through(V3, V3),
    /// A new riser `a..b` (a below), ending on the stacks below and above at these joints.
    New {
        a: V3,
        b: V3,
        below: Option<V3>,
        above: Option<V3>,
    },
}

/// The riser of `system` asked at `at` over `z0..z1`, its branches leaving at `layer`. Stacks of
/// the system within `reach` in plan count as the same column (nearest first).
pub(super) fn join_riser(
    stacks: &[Stack],
    system: &str,
    at: V2,
    reach: f64,
    (z0, z1): (f64, f64),
    layer: f64,
) -> Join {
    let mut near: Vec<&Stack> = stacks
        .iter()
        .filter(|s| s.system == system && (s.at - at).len() <= reach + EPS)
        .collect();
    near.sort_by(|a, b| (a.at - at).len().total_cmp(&(b.at - at).len()));
    if let Some(s) = near.iter().find(|s| s.z0 <= layer + EPS && s.z1 >= layer - EPS) {
        return Join::Through(v3(s.at.x, s.at.y, s.z0), v3(s.at.x, s.at.y, s.z1));
    }
    // a stack ending inside the span sets the axis: the new riser continues it
    let ends_in = |s: &Stack| (s.z1 >= z0 - EPS && s.z1 <= z1) || (s.z0 >= z0 && s.z0 <= z1 + EPS);
    let p = near.iter().find(|s| ends_in(s)).map_or(at, |s| s.at);
    let (mut lo, mut hi) = (z0, z1);
    let (mut below, mut above) = (None, None);
    for s in near.iter().filter(|s| (s.at - p).len() < EPS) {
        if s.z0 < lo && s.z1 >= lo - EPS && s.z1 < hi {
            lo = s.z1;
            below = Some(v3(p.x, p.y, lo));
        }
        if s.z1 > hi && s.z0 <= hi + EPS && s.z0 > lo {
            hi = s.z0;
            above = Some(v3(p.x, p.y, hi));
        }
    }
    Join::New {
        a: v3(p.x, p.y, lo),
        b: v3(p.x, p.y, hi),
        below,
        above,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(x: f64, z0: f64, z1: f64) -> Stack {
        Stack {
            system: "Cold".into(),
            at: v2(x, 0.0),
            z0,
            z1,
        }
    }

    #[test]
    fn a_riser_stands_on_the_one_below_instead_of_overlapping_it() {
        // the storey below ends 0.13 m above where this one starts
        let below = st(1.0, -0.4, 2.93);
        let j = join_riser(&[below], "Cold", v2(1.0, 0.0), 0.5, (2.8, 6.0), 6.0);
        let Join::New { a, b, below, above } = j else { panic!("{j:?}") };
        assert!((a.z - 2.93).abs() < 1e-9 && (b.z - 6.0).abs() < 1e-9);
        assert_eq!(below, Some(v3(1.0, 0.0, 2.93)));
        assert_eq!(above, None);
    }

    #[test]
    fn a_riser_through_the_layer_is_tapped_and_others_are_ignored() {
        let s = [st(1.2, -1.0, 20.0)];
        let j = join_riser(&s, "Cold", v2(1.0, 0.0), 0.5, (2.8, 6.0), 5.7);
        assert_eq!(j, Join::Through(v3(1.2, 0.0, -1.0), v3(1.2, 0.0, 20.0)));
        // another system, or out of reach: a new riser where asked
        for (sys, at) in [("Hot", v2(1.0, 0.0)), ("Cold", v2(2.0, 0.0))] {
            let j = join_riser(&s, sys, at, 0.5, (2.8, 6.0), 5.7);
            assert_eq!(
                j,
                Join::New { a: v3(at.x, 0.0, 2.8), b: v3(at.x, 0.0, 6.0), below: None, above: None }
            );
        }
    }

    #[test]
    fn a_riser_under_one_above_snaps_to_its_axis_and_stops_at_it() {
        let s = [st(1.1, 5.9, 9.0)];
        let j = join_riser(&s, "Cold", v2(1.0, 0.0), 0.5, (2.8, 6.0), 5.7);
        let Join::New { a, b, below, above } = j else { panic!("{j:?}") };
        assert_eq!((a, b), (v3(1.1, 0.0, 2.8), v3(1.1, 0.0, 5.9)));
        assert_eq!((below, above), (None, Some(v3(1.1, 0.0, 5.9))));
    }

    #[test]
    fn only_vertical_pieces_across_a_level_are_risers() {
        let lv = [0.0, 3.2];
        assert!(stack_of("C", v3(0.0, 0.0, -0.4), v3(0.0, 0.0, 2.9), &lv).is_some());
        // a drop to a fixture inside a storey, and a sloped piece
        assert!(stack_of("C", v3(0.0, 0.0, 2.9), v3(0.0, 0.0, 0.6), &lv).is_none());
        assert!(stack_of("C", v3(0.0, 0.0, -0.4), v3(0.5, 0.0, 2.9), &lv).is_none());
    }
}
