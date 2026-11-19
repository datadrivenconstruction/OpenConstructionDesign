// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Steel frames grown from one rule each: a branching tree whose crown carries a deck ring,
//! and a stadium roof ring carried on masts.
//! Every bar is a structural member of the frame, so the model keeps them as columns, beams
//! and braces.

use ddc_geom::steel::lattice::{Frame, MemRole};
use ddc_geom::{v3, V3};
use std::f64::consts::TAU;

/// Lean of the first branches from the vertical and its growth per generation, radians.
const TREE_LEAN0: f64 = 0.45;
const TREE_LEAN_STEP: f64 = 0.15;
const TREE_LEAN_MAX: f64 = 1.0;
/// Sections by generation, trunk first; deeper generations keep the last.
const TREE_SECS: [&str; 5] = ["CHS 323.9x12.5", "CHS 219.1x10", "CHS 168.3x8", "CHS 114.3x6", "CHS 88.9x5"];
/// Edge beam of the crown deck.
const CROWN_SEC: &str = "IPE 200";

/// Stadium ring sections: inner compression ring, outer ring, radial girders, masts, stays,
/// plan bracing of the roof.
const RING_INNER: &str = "HEB 500";
const RING_OUTER: &str = "HEB 300";
const RING_GIRDER: &str = "HEB 400";
const RING_MAST: &str = "CHS 323.9x12.5";
const RING_STAY: &str = "CHS 168.3x8";
const RING_PLAN: &str = "CHS 88.9x5";
/// Rise of the outer edge over the inner ring per metre of roof depth.
const RING_RAKE: f64 = 0.1;
/// Mast head above the outer ring, share of the roof depth.
const MAST_HEAD: f64 = 0.4;
/// Back-stay anchor beyond the mast foot, share of the mast height.
const ANCHOR_OUT: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ring {
    /// Elliptic opening of the inner ring, m.
    pub length: f64,
    pub width: f64,
    /// Plan depth of the roof from the inner to the outer ring, m.
    pub depth: f64,
    /// Inner ring above the ground, m.
    pub height: f64,
    pub masts: usize,
    /// Radial girders per mast.
    pub per_mast: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tree {
    /// Ground to the crown, m.
    pub height: f64,
    pub generations: usize,
    /// Branches out of every tip.
    pub branches: usize,
    /// Length of a branch over its parent's.
    pub ratio: f64,
}

fn lean(g: usize) -> f64 {
    (TREE_LEAN0 + TREE_LEAN_STEP * (g as f64 - 1.0)).min(TREE_LEAN_MAX)
}

/// A vertical trunk (column) splits into `branches` struts per tip for `generations`, each
/// `ratio` times its parent and leaning out within the parent's share of the plan angle, so
/// no two branches meet; the last tips all end at `height` and a ring of beams ties them as
/// the edge of the crown deck.
pub fn tree(t: &Tree) -> Result<Frame, String> {
    if !(t.height > 0.0) || t.generations == 0 || t.branches < 2 || !(t.ratio > 0.0 && t.ratio < 1.0) {
        return Err("tree: height > 0, at least one generation, two branches and 0 < ratio < 1".into());
    }
    let rise = 1.0 + (1..=t.generations).map(|g| t.ratio.powi(g as i32) * lean(g).cos()).sum::<f64>();
    let trunk = t.height / rise;
    let mut f = Frame::new();
    let top = v3(0.0, 0.0, trunk);
    f.add(v3(0.0, 0.0, 0.0), top, TREE_SECS[0], MemRole::Column, v3(1.0, 0.0, 0.0));
    let n = t.branches as f64;
    // (tip, plan angle at the centre of its share)
    let mut tips: Vec<(V3, f64)> = vec![(top, 0.0)];
    for g in 1..=t.generations {
        let len = trunk * t.ratio.powi(g as i32);
        let share = TAU / n.powi(g as i32);
        let (s, c) = lean(g).sin_cos();
        let sec = TREE_SECS[g.min(TREE_SECS.len() - 1)];
        let mut next = Vec::with_capacity(tips.len() * t.branches);
        for &(p, az) in &tips {
            for k in 0..t.branches {
                let a = if g == 1 { share * k as f64 } else { az + (k as f64 - (n - 1.0) / 2.0) * share };
                let q = p + v3(s * a.cos(), s * a.sin(), c) * len;
                f.add(p, q, sec, MemRole::Strut, v3(-a.sin(), a.cos(), 0.0));
                next.push((q, a));
            }
        }
        tips = next;
    }
    let mut crown: Vec<V3> = tips.into_iter().map(|t| t.0).collect();
    crown.sort_by(|a, b| a.y.atan2(a.x).total_cmp(&b.y.atan2(b.x)));
    for i in 0..crown.len() {
        f.add(crown[i], crown[(i + 1) % crown.len()], CROWN_SEC, MemRole::Beam, v3(0.0, 0.0, 1.0));
    }
    Ok(f)
}

/// A roof ring round an elliptic opening: inner compression ring and outer ring of beams tied
/// by radial girders and plan diagonals; every `per_mast`-th outer node sits on a mast
/// (two columns: foot to ring, ring to head) whose head stays the inner ring at its girder and
/// both neighbours and is anchored back to the ground.
pub fn stadium_ring(r: &Ring) -> Result<Frame, String> {
    let ok = [r.length, r.width, r.depth, r.height].iter().all(|x| x.is_finite() && *x > 0.0);
    if !ok || r.masts < 3 || r.per_mast == 0 {
        return Err("stadium ring: positive sizes, at least three masts and one girder per mast".into());
    }
    let n = r.masts * r.per_mast;
    let (a, b) = (r.length / 2.0, r.width / 2.0);
    let zo = r.height + RING_RAKE * r.depth;
    let head = zo + MAST_HEAD * r.depth;
    let at = |i: usize, out: f64, z: f64| {
        let t = TAU * (i % n) as f64 / n as f64;
        v3((a + out) * t.cos(), (b + out) * t.sin(), z)
    };
    let up = v3(0.0, 0.0, 1.0);
    let mut f = Frame::new();
    for i in 0..n {
        let (pi, po) = (at(i, 0.0, r.height), at(i, r.depth, zo));
        f.add(pi, at(i + 1, 0.0, r.height), RING_INNER, MemRole::Girder, up);
        f.add(po, at(i + 1, r.depth, zo), RING_OUTER, MemRole::Beam, up);
        f.add(pi, po, RING_GIRDER, MemRole::Girder, up);
        f.add(pi, at(i + 1, r.depth, zo), RING_PLAN, MemRole::PlanBrace, up);
        if i % r.per_mast != 0 {
            continue;
        }
        let (foot, top) = (at(i, r.depth, 0.0), at(i, r.depth, head));
        let side = v3(-po.y, po.x, 0.0);
        f.add(foot, po, RING_MAST, MemRole::Column, side);
        f.add(po, top, RING_MAST, MemRole::Column, side);
        for k in [i + n - 1, i, i + 1] {
            f.add(top, at(k, 0.0, r.height), RING_STAY, MemRole::Strut, side);
        }
        f.add(top, at(i, r.depth + ANCHOR_OUT * head, 0.0), RING_STAY, MemRole::Strut, side);
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g18() -> Tree {
        Tree { height: 12.0, generations: 4, branches: 3, ratio: 0.7 }
    }

    #[test]
    fn a_tree_of_four_generations_of_three_has_every_branch_and_a_crown_ring() {
        let f = tree(&g18()).unwrap();
        assert!(f.validate().is_empty(), "{:?}", f.validate());
        let count = |r: MemRole| f.members.iter().filter(|m| m.role == r).count();
        assert_eq!(count(MemRole::Column), 1);
        assert_eq!(count(MemRole::Strut), 3 + 9 + 27 + 81);
        assert_eq!(count(MemRole::Beam), 81);
        let top = f.nodes.iter().map(|p| p.z).fold(f64::MIN, f64::max);
        assert!((top - 12.0).abs() < 1e-6, "{top}");
        let crown = f.nodes.iter().filter(|p| (p.z - 12.0).abs() < 1e-6).count();
        assert_eq!(crown, 81);
    }

    #[test]
    fn each_branch_is_shorter_than_its_parent_by_the_ratio() {
        let f = tree(&g18()).unwrap();
        let len = |r: MemRole| f.members.iter().filter(|m| m.role == r).map(|m| f.length(m)).fold(0.0, f64::max);
        let trunk = len(MemRole::Column);
        let mut lens: Vec<f64> = f.members.iter().filter(|m| m.role == MemRole::Strut).map(|m| f.length(m)).collect();
        lens.sort_by(|a, b| b.total_cmp(a));
        lens.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        assert_eq!(lens.len(), 4, "{lens:?}");
        for (g, l) in lens.iter().enumerate() {
            assert!((l / trunk - 0.7f64.powi(g as i32 + 1)).abs() < 1e-6, "{g}: {l}");
        }
    }

    fn g19() -> Ring {
        Ring { length: 150.0, width: 110.0, depth: 30.0, height: 25.0, masts: 16, per_mast: 2 }
    }

    #[test]
    fn a_stadium_ring_has_its_rings_girders_masts_and_stays() {
        let f = stadium_ring(&g19()).unwrap();
        assert!(f.validate().is_empty(), "{:?}", f.validate());
        let count = |r: MemRole| f.members.iter().filter(|m| m.role == r).count();
        // 32 girders: inner ring and radials as girders, outer ring as beams, two columns a mast
        assert_eq!(count(MemRole::Girder), 64);
        assert_eq!(count(MemRole::Beam), 32);
        assert_eq!(count(MemRole::PlanBrace), 32);
        assert_eq!(count(MemRole::Column), 32);
        assert_eq!(count(MemRole::Strut), 16 * 4);
        let feet = f.nodes.iter().filter(|p| p.z.abs() < 1e-9).count();
        assert_eq!(feet, 32, "mast feet and back-stay anchors");
        // masts stand outside the opening of the roof
        for m in f.members.iter().filter(|m| m.role == MemRole::Column) {
            let p = f.nodes[m.a as usize];
            assert!((p.x / 105.0).powi(2) + (p.y / 85.0).powi(2) > 0.99, "{p:?}");
        }
    }

    #[test]
    fn a_stadium_ring_out_of_range_is_refused() {
        assert!(stadium_ring(&Ring { masts: 2, ..g19() }).is_err());
        assert!(stadium_ring(&Ring { per_mast: 0, ..g19() }).is_err());
        assert!(stadium_ring(&Ring { depth: 0.0, ..g19() }).is_err());
    }

    #[test]
    fn a_tree_out_of_range_is_refused() {
        assert!(tree(&Tree { ratio: 1.0, ..g18() }).is_err());
        assert!(tree(&Tree { branches: 1, ..g18() }).is_err());
        assert!(tree(&Tree { generations: 0, ..g18() }).is_err());
    }
}
