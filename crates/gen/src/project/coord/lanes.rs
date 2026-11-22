// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The void order of the cleaned runs: a run crossing a system that ranks above it in the void
//! (gravity drain, ducts, pressure pipes, trays, conduits, sprinkler) moves under it, and the
//! long runs along a corridor take fixed lanes across it, ordered by tier and size, the same
//! order on every corridor of the floor. Every move goes through [`slide`].

use std::collections::BTreeMap;

use super::super::rules::rule;
use super::super::{GProject, RoomKind};
use super::metrics::{cross, level, norm, one_void, sub};
use super::pass::{cells, slide, World};
use super::segs::Kind;

type P3 = (f64, f64, f64);

/// Crossings of line `i` in the wrong void order: the other line, the segment of `i` and the
/// height `i` should clear.
fn wrong(w: &World, p: &GProject, i: usize) -> Vec<(usize, usize, f64)> {
    let l = &w.ls[i];
    let mut out = vec![];
    for (k, s) in l.pts.windows(2).enumerate() {
        if !level(s[0], s[1]) {
            continue;
        }
        let b = [
            s[0].0.min(s[1].0),
            s[0].1.min(s[1].1),
            0.0,
            s[0].0.max(s[1].0),
            s[0].1.max(s[1].1),
            0.0,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for c in cells(&b) {
            for &id in w.grid.get(&c).into_iter().flatten() {
                if !w.alive[id] || !seen.insert(id) {
                    continue;
                }
                let (j, t0, t1, _) = w.segs[id];
                let o = &w.ls[j];
                if j == i
                    || o.storey != l.storey
                    || o.key == l.key
                    || o.tier == l.tier
                    || !level(t0, t1)
                {
                    continue;
                }
                let Some((u, v)) = cross(s[0], s[1], t0, t1) else {
                    continue;
                };
                let zi = s[0].2 + (s[1].2 - s[0].2) * u;
                let zj = t0.2 + (t1.2 - t0.2) * v;
                // the lower tier number runs above
                let order =
                    (o.tier < l.tier && zj < zi - 1e-3) || (o.tier > l.tier && zj > zi + 1e-3);
                if order && one_void(p, (zi, l.size.1), (zj, o.size.1)) {
                    out.push((j, k, zj));
                }
            }
        }
    }
    out.sort_by(|a, b| (a.1, a.0).cmp(&(b.1, b.0)).then(a.2.total_cmp(&b.2)));
    out
}

/// The level stretch of line `i` around segment `k` at one height: its first and last point.
fn stretch(w: &World, i: usize, k: usize) -> (usize, usize) {
    let p = &w.ls[i].pts;
    let z = p[k].2;
    let flat = |a: usize| (p[a].2 - z).abs() < 1e-6 && (p[a + 1].2 - z).abs() < 1e-6;
    let (mut a, mut b) = (k, k);
    while a > 0 && flat(a - 1) {
        a -= 1;
    }
    while b + 2 < p.len() && flat(b + 1) {
        b += 1;
    }
    (a, b + 1)
}

/// Moves runs that cross a higher system above it under it, or the higher one over it.
pub(super) fn tiers(w: &mut World, p: &GProject) -> Vec<usize> {
    let gap = rule("mep.coord.clear_pipe");
    let mut changed = vec![];
    let n = w.ls.len();
    for i in 0..n {
        if w.ls[i].fixed() {
            continue;
        }
        for _ in 0..4 {
            let bad = wrong(w, p, i);
            let Some(&(j, k, zj)) = bad.first() else {
                break;
            };
            let (a, b) = stretch(w, i, k);
            let (li, lj) = (&w.ls[i], &w.ls[j]);
            let half = (li.size.1 + lj.size.1) / 2.0 + gap;
            let z = li.pts[k].2;
            let target = if lj.tier < li.tier {
                zj - half
            } else {
                zj + half
            };
            let s = rule("mep.coord.snap");
            // under a higher system: snap away from it
            let target = if lj.tier < li.tier {
                (target / s).floor() * s
            } else {
                (target / s).ceil() * s
            };
            let before = bad.len();
            let mut moved = false;
            // the runs hanging on this one may follow it: they must not get worse either
            let hang: Vec<usize> = w
                .touching(i, &w.ls[i].pts.clone())
                .into_iter()
                .map(|x| x.0)
                .collect();
            let hang_before: usize = hang.iter().map(|&f| wrong(w, p, f).len()).sum();
            if a >= 1 && b + 2 <= w.ls[i].pts.len() && slide(w, i, a, b, (0.0, 0.0, target - z)) {
                let hang_after: usize = hang.iter().map(|&f| wrong(w, p, f).len()).sum();
                if wrong(w, p, i).len() < before && hang_after <= hang_before {
                    moved = true;
                } else {
                    let back = z - w.ls[i].pts[a].2;
                    slide(w, i, a, b, (0.0, 0.0, back));
                }
            }
            if !moved {
                break;
            }
            changed.push(i);
        }
    }
    changed.sort_unstable();
    changed.dedup();
    changed
}

/// Rank of a system across a corridor, from the first lane.
fn lane_key(w: &World, i: usize) -> (u8, i64, String) {
    let l = &w.ls[i];
    (l.tier, -((l.size.0 * 1e3).round() as i64), l.key.clone())
}

/// Corridor outlines of storey `k` as boxes: x0, y0, x1, y1.
fn corridors(p: &GProject, k: usize) -> Vec<[f64; 4]> {
    let st = &p.storeys[k];
    st.rooms
        .iter()
        .enumerate()
        .filter(|(_, r)| r.kind == RoomKind::Corridor)
        .filter_map(|(ri, _)| st.layout.rooms.get(ri))
        .map(|r| {
            r.polygon
                .iter()
                .fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, q| {
                    [b[0].min(q.0), b[1].min(q.1), b[2].max(q.0), b[3].max(q.1)]
                })
        })
        .collect()
}

/// Lays the long runs of each corridor into lanes: systems in a fixed order across it, each
/// lane a set distance from the last, the bus centred and snapped to the grid.
pub(super) fn lanes(w: &mut World, p: &GProject, origin: (f64, f64)) -> Vec<usize> {
    let s = rule("mep.coord.snap");
    let gap = rule("mep.coord.clear_duct");
    let least = rule("mep.coord.lane_min");
    let mut changed = vec![];
    for k in 0..p.storeys.len() {
        for c in corridors(p, k) {
            let along_x = c[2] - c[0] >= c[3] - c[1];
            let (lo, hi) = if along_x { (c[1], c[3]) } else { (c[0], c[2]) };
            let inside = |q: P3| {
                q.0 > c[0] - 1e-3 && q.0 < c[2] + 1e-3 && q.1 > c[1] - 1e-3 && q.1 < c[3] + 1e-3
            };
            // the long straight of each movable line along the corridor
            let mut items: Vec<(usize, usize, f64, f64)> = vec![];
            for (i, l) in w.ls.iter().enumerate() {
                if l.storey != k || l.fixed() || l.kind == Kind::Conduit || l.pts.len() < 4 {
                    continue;
                }
                let best = l
                    .pts
                    .windows(2)
                    .enumerate()
                    .skip(1)
                    .take(l.pts.len().saturating_sub(3))
                    .filter(|(_, q)| inside(q[0]) && inside(q[1]) && (q[0].2 - q[1].2).abs() < 1e-6)
                    .filter(|(_, q)| {
                        if along_x {
                            (q[0].1 - q[1].1).abs() < 1e-6
                        } else {
                            (q[0].0 - q[1].0).abs() < 1e-6
                        }
                    })
                    .map(|(sk, q)| (sk, norm(sub(q[1], q[0]))))
                    .filter(|x| x.1 >= least)
                    .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
                if let Some((sk, _)) = best {
                    let q = l.pts[sk];
                    let across = if along_x { q.1 } else { q.0 };
                    let width = l.size.0;
                    items.push((i, sk, across, width));
                }
            }
            if items.len() < 2 {
                continue;
            }
            items.sort_by(|a, b| (lane_key(w, a.0), a.0).cmp(&(lane_key(w, b.0), b.0)));
            let total: f64 =
                items.iter().map(|x| x.3).sum::<f64>() + gap * (items.len() - 1) as f64;
            if total > hi - lo - 2.0 * gap {
                continue;
            }
            // lanes from the side the first system already runs nearer
            let first = items[0].2;
            let from_lo = first - lo <= hi - first;
            let o = if along_x { origin.1 } else { origin.0 };
            let mut at = if from_lo {
                (lo + hi) / 2.0 - total / 2.0
            } else {
                (lo + hi) / 2.0 + total / 2.0
            };
            let mut plan: BTreeMap<usize, (usize, f64)> = BTreeMap::new();
            for &(i, sk, _, width) in &items {
                let c = if from_lo {
                    at + width / 2.0
                } else {
                    at - width / 2.0
                };
                let c = o + ((c - o) / s).round() * s;
                plan.insert(i, (sk, c));
                at = if from_lo {
                    c + width / 2.0 + gap
                } else {
                    c - width / 2.0 - gap
                };
            }
            for (i, (sk, c)) in plan {
                let q = w.ls[i].pts[sk];
                let cur = if along_x { q.1 } else { q.0 };
                let d = c - cur;
                if d.abs() < 1e-6 {
                    continue;
                }
                let v = if along_x {
                    (0.0, d, 0.0)
                } else {
                    (d, 0.0, 0.0)
                };
                if slide(w, i, sk, sk + 1, v) {
                    changed.push(i);
                }
            }
        }
    }
    changed.sort_unstable();
    changed.dedup();
    changed
}
