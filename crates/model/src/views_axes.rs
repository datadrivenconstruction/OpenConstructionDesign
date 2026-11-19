// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Axis chain of a plan's implicit dimensions: axis to axis along each side of the extent, set
//! between the opening chain and the overall size, which moves out one tier. With three or more
//! axes the first-to-last axis size stands outside the bays, the overall size outside that.

use crate::views::{Side, ViewPrim};

/// Positions of the axes square to one side, inside the extent `[lo, hi]`, sorted, once each.
fn axes(prims: &[ViewPrim], vertical: bool, lo: f64, hi: f64) -> Vec<f64> {
    let mut v: Vec<f64> = prims
        .iter()
        .filter_map(|p| match p {
            ViewPrim::Grid { a, b, .. } => {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let l = dx.hypot(dy);
                if l < 1e-9 {
                    None
                } else if vertical && (dx / l).abs() < 1e-6 {
                    Some(a.0)
                } else if !vertical && (dy / l).abs() < 1e-6 {
                    Some(a.1)
                } else {
                    None
                }
            }
            _ => None,
        })
        .filter(|t| *t >= lo - 1e-6 && *t <= hi + 1e-6)
        .collect();
    v.sort_by(f64::total_cmp);
    v.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    v
}

/// Adds the axis chains (tier 1) to the implicit chains of `prims`; the overall size of a side
/// with an axis chain goes to tier 2. Sides with fewer than two axes keep their two tiers.
pub(crate) fn grid_chains(prims: &mut Vec<ViewPrim>, bb: [f64; 4]) {
    let xs = axes(prims, true, bb[0], bb[2]);
    let ys = axes(prims, false, bb[1], bb[3]);
    let mut add = vec![];
    for side in [Side::South, Side::North, Side::West, Side::East] {
        let horiz = matches!(side, Side::South | Side::North);
        let pts = if horiz { &xs } else { &ys };
        if pts.len() < 2 {
            continue;
        }
        let c = match side {
            Side::South => bb[1],
            Side::North => bb[3],
            Side::West => bb[0],
            Side::East => bb[2],
        };
        let pt = |t: f64| if horiz { (t, c) } else { (c, t) };
        let span = pts.len() >= 3;
        for p in prims.iter_mut() {
            if let ViewPrim::Dim { side: s, chain, .. } = p {
                if *s == side && *chain == 1 {
                    *chain = if span { 3 } else { 2 };
                }
            }
        }
        if span {
            let (lo, hi) = (pts[0], pts[pts.len() - 1]);
            add.push(ViewPrim::Dim { side, chain: 2, a: pt(lo), b: pt(hi), value: hi - lo });
        }
        for w in pts.windows(2) {
            add.push(ViewPrim::Dim {
                side,
                chain: 1,
                a: pt(w[0]),
                b: pt(w[1]),
                value: w[1] - w[0],
            });
        }
    }
    prims.extend(add);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(x0: f64, y0: f64, x1: f64, y1: f64) -> ViewPrim {
        ViewPrim::Grid {
            label: String::new(),
            a: (x0, y0),
            b: (x1, y1),
            bubbles: (true, true),
        }
    }

    #[test]
    fn axis_chain_sits_between_openings_and_overall() {
        let bb = [0.0, 0.0, 12.0, 6.0];
        let mut p = vec![
            ViewPrim::Dim {
                side: Side::South,
                chain: 1,
                a: (0.0, 0.0),
                b: (12.0, 0.0),
                value: 12.0,
            },
            grid(0.2, -2.0, 0.2, 8.0),
            grid(6.0, -2.0, 6.0, 8.0),
            grid(11.8, -2.0, 11.8, 8.0),
            grid(-2.0, 3.0, 14.0, 3.0),
        ];
        grid_chains(&mut p, bb);
        let tier = |s: Side, c: u8| {
            p.iter()
                .filter(|q| {
                    matches!(q, ViewPrim::Dim { side, chain, .. } if *side == s && *chain == c)
                })
                .count()
        };
        assert_eq!(tier(Side::South, 2), 1, "first to last axis");
        assert_eq!(tier(Side::South, 3), 1, "overall moved out twice");
        // the fixture has no North overall size to move out, only the axis span
        assert_eq!(tier(Side::North, 2), 1);
        assert_eq!(tier(Side::North, 3), 0);
        assert_eq!(tier(Side::West, 2), 0, "one axis: no span");
        assert_eq!(tier(Side::South, 1), 2, "two bays");
        assert_eq!(tier(Side::North, 1), 2);
        assert_eq!(tier(Side::West, 1), 0, "one axis: no chain");
        let total: f64 = p
            .iter()
            .filter_map(|q| match q {
                ViewPrim::Dim { side: Side::South, chain: 1, value, .. } => Some(*value),
                _ => None,
            })
            .sum();
        assert!((total - 11.6).abs() < 1e-9);
    }
}
