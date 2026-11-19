// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Railing detail: round handrail mitred along the path, posts with base plates, a bottom rail
//! and round balusters. Coarse keeps posts and the handrail only; balusters appear from medium.

use super::prim::{block, chamfered, prism, rod, tube, X, Y, Z};
use super::rules::DetailRules;
use super::{round_sides, Detail, Lod, Mat};
use crate::math::{v3, V3};
use crate::stair::Railing;

/// Bottom rail above the path, plate size and thickness under a post.
const BOTTOM_RAIL: f64 = 0.1;
const PLATE: f64 = 0.1;
const PLATE_T: f64 = 0.01;

/// Sizes of a new railing from the rules: `(height, post spacing, baluster spacing, post,
/// baluster, handrail diameter)`. A stair flight takes the handrail height, a free edge the
/// guard height.
pub fn sizes(r: &DetailRules, stair: bool) -> (f64, f64, f64, f64, f64, f64) {
    let baluster = 0.02;
    (
        if stair { r.handrail_height } else { r.guard_height },
        r.post_spacing,
        r.baluster_gap + baluster,
        0.05,
        baluster,
        r.handrail_d,
    )
}

pub fn build(rl: &Railing, lod: Lod) -> Detail {
    let mut d = Detail::default();
    if rl.check().is_err() {
        return d;
    }
    let l = rl.layout();
    // the round handrail spans the larger side of the rail section: width and height both show
    let rd = rl.rail.0.max(rl.rail.1).max(0.02);
    let top = rl.height - rd / 2.0;
    let up = |p: V3, z: f64| p + Z * z;
    let m = d.part(Mat::Metal);
    // handrail over the whole path, its ends returned down to the end posts at fine
    let rail: Vec<V3> = rl.path.iter().map(|p| up(*p, top)).collect();
    tube(m, &rail, rd / 2.0, round_sides(lod, false));
    let post = rl.post.max(0.02);
    for p in &l.posts {
        let sec = if lod == Lod::Coarse {
            chamfered(-post / 2.0, -post / 2.0, post / 2.0, post / 2.0, 0.0)
        } else {
            chamfered(-post / 2.0, -post / 2.0, post / 2.0, post / 2.0, post * 0.12)
        };
        prism(m, &sec, *p, X, Y, Z * (top - rd / 2.0));
        if lod == Lod::Fine {
            let h = PLATE / 2.0;
            block(m, v3(p.x - h, p.y - h, p.z), v3(p.x + h, p.y + h, p.z + PLATE_T));
        }
    }
    if lod == Lod::Coarse {
        return d;
    }
    let br: Vec<V3> = rl.path.iter().map(|p| up(*p, BOTTOM_RAIL)).collect();
    tube(m, &br, (rl.baluster * 0.8).max(0.008), round_sides(lod, true));
    let n = round_sides(lod, true);
    for b in &l.balusters {
        rod(m, up(*b, BOTTOM_RAIL), up(*b, top - rd / 2.0), rl.baluster / 2.0, n);
    }
    d
}

/// Triangles per metre of path.
pub fn per_metre(d: &Detail, rl: &Railing) -> f64 {
    let len: f64 = rl.path.windows(2).map(|w| (w[1] - w[0]).len()).sum();
    d.triangles() as f64 / len.max(1e-9)
}
