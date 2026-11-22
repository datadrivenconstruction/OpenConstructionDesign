// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! How coordinated the services look, per system: path efficiency, bends per 10 m, fittings
//! other than 90 and 45 degrees, plan crossings, level changes, crossings in the wrong tier
//! order, hard and soft clashes by pair, straights shorter than the least segment, and a hash
//! of the sorted segments. Every number comes from integer millimetres relative to the project
//! origin, so the same design gives the same JSON wherever it stands.

use std::collections::BTreeMap;

use serde_json::{json, Value as J};
use sha2::{Digest, Sha256};

use super::super::rules::rule;
use super::super::GProject;
use super::segs::{lines, Kind, Line};

type P3 = (f64, f64, f64);

/// A turn sharper than this is a bend, degrees: drain falls of a few percent are not.
const STRAIGHT_DEG: f64 = 2.5;
/// A fitting within this of 45 or 90 degrees counts as one, degrees.
const FIT_TOL_DEG: f64 = 2.5;
/// A segment rising more than this fraction of its length changes level.
const STEEP: f64 = 0.2;

pub(crate) fn sub(a: P3, b: P3) -> P3 {
    (a.0 - b.0, a.1 - b.1, a.2 - b.2)
}
fn dot(a: P3, b: P3) -> f64 {
    a.0 * b.0 + a.1 * b.1 + a.2 * b.2
}
pub(crate) fn norm(a: P3) -> f64 {
    dot(a, a).sqrt()
}

/// Turn at the middle point, degrees: 0 straight on.
pub(crate) fn turn(a: P3, b: P3, c: P3) -> f64 {
    let (u, v) = (sub(b, a), sub(c, b));
    let (lu, lv) = (norm(u), norm(v));
    if lu < 1e-9 || lv < 1e-9 {
        return 0.0;
    }
    (dot(u, v) / (lu * lv)).clamp(-1.0, 1.0).acos().to_degrees()
}

pub(crate) fn is_bend(t: f64) -> bool {
    t > STRAIGHT_DEG
}

/// Whether a segment runs level: drains with their fall count as level.
pub(crate) fn level(a: P3, b: P3) -> bool {
    let h = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    (b.2 - a.2).abs() <= STEEP * h
}

/// Least straight between two fittings, m: pipes and conduits, then ducts and trays.
pub(crate) fn min_seg(kind: Kind) -> f64 {
    match kind {
        Kind::Pipe | Kind::Conduit => rule("mep.coord.min_pipe"),
        Kind::Duct | Kind::Tray => rule("mep.coord.min_duct"),
    }
}

fn length(p: &[P3]) -> f64 {
    p.windows(2).map(|w| norm(sub(w[1], w[0]))).sum()
}

/// Bends of a polyline.
pub(crate) fn bends(p: &[P3]) -> usize {
    p.windows(3)
        .filter(|w| is_bend(turn(w[0], w[1], w[2])))
        .count()
}

/// Level segments between two bends shorter than the least segment.
pub(crate) fn short_segs(p: &[P3], kind: Kind) -> usize {
    let n = p.len();
    if n < 4 {
        return 0;
    }
    let m = min_seg(kind) - 1e-4;
    (1..n - 2)
        .filter(|&k| level(p[k], p[k + 1]) && norm(sub(p[k + 1], p[k])) < m)
        .filter(|&k| {
            is_bend(turn(p[k - 1], p[k], p[k + 1])) && is_bend(turn(p[k], p[k + 1], p[k + 2]))
        })
        .count()
}

fn odd_fittings(p: &[P3]) -> usize {
    p.windows(3)
        .map(|w| turn(w[0], w[1], w[2]))
        .filter(|&t| is_bend(t) && (t - 90.0).abs() > FIT_TOL_DEG && (t - 45.0).abs() > FIT_TOL_DEG)
        .count()
}

/// Interior segments that climb or drop: the first and last legs reach terminals and risers.
fn dz_changes(p: &[P3]) -> usize {
    let n = p.len();
    if n < 4 {
        return 0;
    }
    (1..n - 2)
        .filter(|&k| !level(p[k], p[k + 1]) && (p[k + 1].2 - p[k].2).abs() > 0.02)
        .count()
}

/// Plan crossing of two level segments away from their ends: the point and the share along each.
pub(crate) fn cross(a: P3, b: P3, c: P3, d: P3) -> Option<(f64, f64)> {
    let (r, s) = ((b.0 - a.0, b.1 - a.1), (d.0 - c.0, d.1 - c.1));
    let den = r.0 * s.1 - r.1 * s.0;
    if den.abs() < 1e-9 {
        return None;
    }
    let q = (c.0 - a.0, c.1 - a.1);
    let t = (q.0 * s.1 - q.1 * s.0) / den;
    let u = (q.0 * r.1 - q.1 * r.0) / den;
    let e = 1e-3;
    (t > e && t < 1.0 - e && u > e && u < 1.0 - e).then_some((t, u))
}

pub(crate) struct Seg {
    pub line: usize,
    pub a: P3,
    pub b: P3,
}

/// Level segments of every line by storey, in a grid of plan cells for the crossing search.
pub(crate) fn level_segs(ls: &[Line]) -> Vec<Seg> {
    let mut out = vec![];
    for (i, l) in ls.iter().enumerate() {
        for w in l.pts.windows(2) {
            if level(w[0], w[1]) && norm(sub(w[1], w[0])) > 1e-6 {
                out.push(Seg {
                    line: i,
                    a: w[0],
                    b: w[1],
                });
            }
        }
    }
    out
}

/// Every plan crossing between segments of different lines on one storey: segment indices
/// and the axis heights there.
pub(crate) fn crossings(ls: &[Line], segs: &[Seg]) -> Vec<(usize, usize, f64, f64)> {
    let cell = 2.0;
    let mut grid: BTreeMap<(usize, i64, i64), Vec<usize>> = BTreeMap::new();
    for (k, s) in segs.iter().enumerate() {
        let st = ls[s.line].storey;
        let (x0, x1) = (s.a.0.min(s.b.0), s.a.0.max(s.b.0));
        let (y0, y1) = (s.a.1.min(s.b.1), s.a.1.max(s.b.1));
        for gx in (x0 / cell).floor() as i64..=(x1 / cell).floor() as i64 {
            for gy in (y0 / cell).floor() as i64..=(y1 / cell).floor() as i64 {
                grid.entry((st, gx, gy)).or_default().push(k);
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut out = vec![];
    for v in grid.values() {
        for (x, &i) in v.iter().enumerate() {
            for &j in &v[x + 1..] {
                let (si, sj) = (&segs[i], &segs[j]);
                if si.line == sj.line || !seen.insert((i, j)) {
                    continue;
                }
                if let Some((t, u)) = cross(si.a, si.b, sj.a, sj.b) {
                    let zi = si.a.2 + (si.b.2 - si.a.2) * t;
                    let zj = sj.a.2 + (sj.b.2 - sj.a.2) * u;
                    out.push((i, j, zi, zj));
                }
            }
        }
    }
    out
}

/// Whether two runs crossing at heights `za` and `zb`, `ha` and `hb` deep, share one void: close
/// enough in height and no slab between them.
pub(crate) fn one_void(p: &GProject, (za, ha): (f64, f64), (zb, hb): (f64, f64)) -> bool {
    let (lo, hi) = if za <= zb { (za, zb) } else { (zb, za) };
    let gap = hi - lo - (ha + hb) / 2.0;
    gap <= rule("mep.coord.tier_band")
        && !p
            .storeys
            .iter()
            .any(|s| s.elevation - super::super::svc::slab() < hi && s.elevation > lo)
}

/// Clearance wanted between two labelled pieces of the void search, m.
pub(crate) fn clearance(a: &str, b: &str) -> f64 {
    let class = |l: &str| {
        if l.starts_with("duct") {
            'd'
        } else if l == "Power" {
            'e'
        } else if matches!(l, "Cold" | "Hot" | "Sanitary" | "sprinkler") {
            'w'
        } else {
            'o'
        }
    };
    match (class(a), class(b)) {
        ('o', _) | (_, 'o') => 0.0,
        ('e', 'w') | ('w', 'e') => rule("mep.coord.clear_elec_wet"),
        ('d', _) | (_, 'd') => rule("mep.coord.clear_duct"),
        _ => rule("mep.coord.clear_pipe"),
    }
}

fn mm(v: f64) -> i64 {
    (v * 1000.0).round() as i64
}

/// The metrics of the services of `p`, built with its origin at `origin`.
pub fn route_metrics(p: &GProject, origin: (f64, f64)) -> J {
    let ls = lines(p);
    #[derive(Default)]
    struct Acc {
        runs: usize,
        segments: usize,
        length: f64,
        manhattan: f64,
        routed: f64,
        bends: usize,
        odd: usize,
        crossings: usize,
        dz: usize,
        tier: usize,
        short: usize,
        hash: Vec<String>,
    }
    let mut by: BTreeMap<String, Acc> = BTreeMap::new();
    let mut all_hash = vec![];
    for l in &ls {
        let a = by.entry(l.key.clone()).or_default();
        let (Some(&f), Some(&e)) = (l.pts.first(), l.pts.last()) else {
            continue;
        };
        a.runs += 1;
        a.segments += l.pts.len().saturating_sub(1);
        a.length += length(&l.pts);
        // routed length along the axes against the shortest such route between the ends
        let d = sub(e, f);
        a.manhattan += d.0.abs() + d.1.abs() + d.2.abs();
        a.routed += l
            .pts
            .windows(2)
            .map(|w| {
                let s = sub(w[1], w[0]);
                s.0.abs() + s.1.abs() + s.2.abs()
            })
            .sum::<f64>();
        a.bends += bends(&l.pts);
        a.odd += odd_fittings(&l.pts);
        a.dz += dz_changes(&l.pts);
        a.short += short_segs(&l.pts, l.kind);
        for w in l.pts.windows(2) {
            let q = |p: P3| (mm(p.0 - origin.0), mm(p.1 - origin.1), mm(p.2));
            let (x, y) = (q(w[0]), q(w[1]));
            let (x, y) = if x <= y { (x, y) } else { (y, x) };
            let s = format!(
                "{}|{}|{},{},{}|{},{},{}",
                l.key, l.storey, x.0, x.1, x.2, y.0, y.1, y.2
            );
            a.hash.push(s.clone());
            all_hash.push(s);
        }
    }
    let segs = level_segs(&ls);
    let mut tier_pairs: BTreeMap<String, usize> = BTreeMap::new();
    let mut cross_pairs: BTreeMap<String, usize> = BTreeMap::new();
    for (i, j, zi, zj) in crossings(&ls, &segs) {
        let (li, lj) = (&ls[segs[i].line], &ls[segs[j].line]);
        if li.key == lj.key {
            continue;
        }
        by.get_mut(&li.key).unwrap().crossings += 1;
        by.get_mut(&lj.key).unwrap().crossings += 1;
        let (hi, lo) = if (li.tier, &li.key) <= (lj.tier, &lj.key) {
            ((li, zi), (lj, zj))
        } else {
            ((lj, zj), (li, zi))
        };
        *cross_pairs
            .entry(format!("{}/{}", hi.0.key, lo.0.key))
            .or_default() += 1;
        // the system higher in the order runs above at a crossing
        if hi.0.tier < lo.0.tier
            && hi.1 < lo.1 - 1e-3
            && one_void(p, (hi.1, hi.0.size.1), (lo.1, lo.0.size.1))
        {
            by.get_mut(&lo.0.key).unwrap().tier += 1;
            *tier_pairs
                .entry(format!("{}/{}", hi.0.key, lo.0.key))
                .or_default() += 1;
        }
    }
    let pad = [
        rule("mep.coord.clear_pipe"),
        rule("mep.coord.clear_duct"),
        rule("mep.coord.clear_elec_wet"),
    ]
    .into_iter()
    .fold(0.0, f64::max);
    let mut hard: BTreeMap<String, usize> = BTreeMap::new();
    let mut soft: BTreeMap<String, usize> = BTreeMap::new();
    for n in super::super::void::near_pairs(p, pad, &clearance) {
        let (x, y) = if n.a.0 <= n.b.0 {
            (&n.a.0, &n.b.0)
        } else {
            (&n.b.0, &n.a.0)
        };
        let key = format!("{x}/{y}");
        if n.depth > super::super::svc::TOL {
            *hard.entry(key).or_default() += 1;
        } else {
            *soft.entry(key).or_default() += 1;
        }
    }
    let hex = |v: &mut Vec<String>| {
        v.sort();
        let mut h = Sha256::new();
        for s in v.iter() {
            h.update(s.as_bytes());
            h.update(b"\n");
        }
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let r3 = |v: f64| (v * 1000.0).round() / 1000.0;
    let mut systems = serde_json::Map::new();
    let (mut tl, mut tm, mut tr, mut tb) = (0.0, 0.0, 0.0, 0usize);
    for (k, a) in by.iter_mut() {
        tl += a.length;
        tm += a.manhattan;
        tr += a.routed;
        tb += a.bends;
        let eff = if a.routed > 0.0 {
            a.manhattan / a.routed
        } else {
            1.0
        };
        let b10 = if a.length > 0.0 {
            a.bends as f64 * 10.0 / a.length
        } else {
            0.0
        };
        systems.insert(
            k.clone(),
            json!({
                "runs": a.runs, "segments": a.segments, "length_m": r3(a.length),
                "efficiency": r3(eff), "bends": a.bends, "bends_per_10m": r3(b10),
                "odd_fittings": a.odd, "crossings": a.crossings, "dz_changes": a.dz,
                "tier_violations": a.tier, "short_segments": a.short, "sha256": hex(&mut a.hash),
            }),
        );
    }
    json!({
        "systems": systems,
        "total": {
            "length_m": r3(tl),
            "efficiency": r3(if tr > 0.0 { tm / tr } else { 1.0 }),
            "bends_per_10m": r3(if tl > 0.0 { tb as f64 * 10.0 / tl } else { 0.0 }),
            "hard_clashes": hard.values().sum::<usize>(),
            "soft_clashes": soft.values().sum::<usize>(),
        },
        "crossings": cross_pairs,
        "tier_violations": tier_pairs,
        "hard_clashes": hard,
        "soft_clashes": soft,
        "sha256": hex(&mut all_hash),
    })
}
