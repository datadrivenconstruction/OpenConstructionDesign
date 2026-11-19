//! Geometry oracles on the members of the saved model: extents, taper, curvature, legs,
//! duplicates, free ends, edit locality, double curvature of a shell.

use super::case::GeomO;
use super::facts::{sample, Cells, Facts, Member, P3};
use super::oracle::{in_range, no_capability, num, range_text, result, EvalCx, Oracle, OracleResult};
use std::collections::HashSet;

pub fn build(g: &GeomO) -> Option<Box<dyn Oracle>> {
    let kind = g.kind.as_str();
    ["height", "taper", "curve_dev", "legs", "no_dup_members", "free_ends", "edit_kept", "new_within", "double_curve", "slab_at"]
        .contains(&kind)
        .then(|| Box::new(Geom(g.clone())) as Box<dyn Oracle>)
}

struct Geom(GeomO);

impl Oracle for Geom {
    fn name(&self) -> String {
        format!("geom {}", self.0.kind)
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let p = &self.0.p;
        let (min, max) = (num(p, "min"), num(p, "max"));
        let name = self.name();
        let f = cx.facts;
        let judged = |measured: Option<f64>, shown: String| -> OracleResult {
            let exp = range_text(min, max);
            match measured {
                Some(v) => result(&name, exp, shown, in_range(v, min, max)),
                None => no_capability(&name, exp, &shown),
            }
        };
        match self.0.kind.as_str() {
            "height" => match height(f) {
                Some(h) => judged(Some(h), format!("{h:.2} m")),
                None => judged(None, "nothing built to measure".into()),
            },
            "taper" => match profile(f) {
                Some((_, taper, _)) => judged(Some(taper), format!("{taper:.3}")),
                None => judged(None, "no members to measure".into()),
            },
            "curve_dev" => match profile(f) {
                Some((_, _, dev)) => judged(Some(dev), format!("{dev:.4}")),
                None => judged(None, "no members to measure".into()),
            },
            "legs" => {
                let n = legs(f);
                judged(Some(n as f64), format!("{n} legs"))
            }
            "no_dup_members" => {
                let d = duplicates(&f.members);
                let exp = "0 duplicated members".to_string();
                result(&name, exp, format!("{d} duplicated"), d == 0)
            }
            "free_ends" => {
                let (free, ends) = free_ends(&f.members, num(p, "tol").unwrap_or(0.3));
                let max = max.unwrap_or(0.0);
                result(&name, format!("<= {max}"), format!("{free} free of {ends} ends"), free as f64 <= max)
            }
            "edit_kept" => match cx.before {
                None => no_capability(&name, range_text(min, None), "case has no base model"),
                Some(b) => {
                    let (kept, total) = kept(&b.members, &f.members);
                    let share = if total == 0 { 1.0 } else { kept as f64 / total as f64 };
                    judged(Some(share), format!("{kept} of {total} members unchanged ({share:.3})"))
                }
            },
            "new_within" => {
                let (Some(zmin), Some(zmax), Some(b)) = (num(p, "zmin"), num(p, "zmax"), cx.before) else {
                    return no_capability(&name, "zmin, zmax and a base model".into(), "missing parameter or base");
                };
                let old: HashSet<_> = b.members.iter().map(key).collect();
                let new: Vec<&Member> = f.members.iter().filter(|m| !old.contains(&key(m))).collect();
                let out = new.iter().filter(|m| m.a[2] < zmin - 1e-6 || m.a[2] > zmax + 1e-6 || m.b[2] < zmin - 1e-6 || m.b[2] > zmax + 1e-6).count();
                let need = num(p, "new_min").unwrap_or(1.0);
                let exp = format!("at least {need} new members, all within z {zmin}..{zmax}");
                let ok = out == 0 && new.len() as f64 >= need;
                result(&name, exp, format!("{} new, {out} outside the band", new.len()), ok)
            }
            "slab_at" => {
                let (Some(zmin), Some(zmax)) = (num(p, "zmin"), num(p, "zmax")) else {
                    return no_capability(&name, "zmin and zmax".into(), "missing parameter");
                };
                let n = f.slab_z.iter().filter(|z| **z >= zmin - 1e-6 && **z <= zmax + 1e-6).count();
                judged(Some(n as f64), format!("{n} slab(s) at z {zmin}..{zmax}"))
            }
            "double_curve" => match double_curve(f) {
                Some((dx, dy)) => {
                    let lo = dx.min(dy);
                    judged(Some(lo), format!("along x {dx:.3}, along y {dy:.3} of span"))
                }
                None => judged(None, "no shell nodes or members to measure".into()),
            },
            _ => unreachable!("build admits only known kinds"),
        }
    }
}

/// Height of everything built: member ends, shell nodes, walls and solids.
pub fn height(f: &Facts) -> Option<f64> {
    let z = || f.points.iter().chain(f.masses.iter().flatten()).map(|p| p[2]);
    if z().next().is_none() {
        return None;
    }
    let lo = z().fold(f64::MAX, f64::min);
    let hi = z().fold(f64::MIN, f64::max);
    Some(if lo >= 0.0 { hi } else { hi - lo })
}

/// Half-width of the structure per height band: (widths, bottom/top ratio, largest departure
/// from the straight line between the bottom and the top width, relative to the bottom width).
pub fn profile(f: &Facts) -> Option<(Vec<f64>, f64, f64)> {
    const BANDS: usize = 6;
    let pts: Vec<P3> = f.members.iter().flat_map(|m| [m.a, m.b]).collect();
    if pts.len() < 20 {
        return None;
    }
    let (zlo, zhi) = (
        pts.iter().map(|p| p[2]).fold(f64::MAX, f64::min),
        pts.iter().map(|p| p[2]).fold(f64::MIN, f64::max),
    );
    if zhi - zlo < 10.0 {
        return None;
    }
    let mid = |i: usize| {
        let v: Vec<f64> = pts.iter().map(|p| p[i]).collect();
        (v.iter().cloned().fold(f64::MAX, f64::min) + v.iter().cloned().fold(f64::MIN, f64::max)) / 2.0
    };
    let (cx, cy) = (mid(0), mid(1));
    let r: Vec<f64> = (0..BANDS)
        .map(|b| {
            let lo = zlo + (zhi - zlo) * b as f64 / BANDS as f64;
            let hi = zlo + (zhi - zlo) * (b + 1) as f64 / BANDS as f64;
            pts.iter()
                .filter(|p| p[2] >= lo && p[2] <= hi + 1e-9)
                .map(|p| (p[0] - cx).abs().max((p[1] - cy).abs()))
                .fold(0.0, f64::max)
        })
        .collect();
    if r[0] <= 0.0 || r[BANDS - 1] <= 0.0 {
        return Some((r, 0.0, 0.0));
    }
    let z = |b: usize| zlo + (zhi - zlo) * (b as f64 + 0.5) / BANDS as f64;
    let dev = (0..BANDS)
        .map(|b| (r[b] - (r[0] + (r[BANDS - 1] - r[0]) * (z(b) - z(0)) / (z(BANDS - 1) - z(0)))).abs())
        .fold(0.0, f64::max)
        / r[0];
    let taper = r[0] / r[BANDS - 1];
    Some((r, taper, dev))
}

/// Separate footprints at the foot of the structure: members reaching the lowest level,
/// grouped by place (within 1 m).
pub fn legs(f: &Facts) -> usize {
    let zmin = f.members.iter().flat_map(|m| [m.a[2], m.b[2]]).fold(f64::MAX, f64::min);
    let mut feet: Vec<P3> = vec![];
    for m in &f.members {
        for p in [m.a, m.b] {
            if p[2] <= zmin + 1.0 && !feet.iter().any(|q| (q[0] - p[0]).hypot(q[1] - p[1]) <= 1.0) {
                feet.push(p);
            }
        }
    }
    feet.len()
}

fn r2(v: f64) -> i64 {
    (v * 100.0).round() as i64
}

type Key = (&'static str, [i64; 3], [i64; 3]);

fn key(m: &Member) -> Key {
    let a = [r2(m.a[0]), r2(m.a[1]), r2(m.a[2])];
    let b = [r2(m.b[0]), r2(m.b[1]), r2(m.b[2])];
    if a <= b {
        (m.kind, a, b)
    } else {
        (m.kind, b, a)
    }
}

pub fn duplicates(members: &[Member]) -> usize {
    let mut seen = HashSet::new();
    members.iter().filter(|m| !seen.insert(key(m))).count()
}

fn kept(before: &[Member], after: &[Member]) -> (usize, usize) {
    let now: HashSet<_> = after.iter().map(key).collect();
    (before.iter().filter(|m| now.contains(&key(m))).count(), before.len())
}

/// Member ends that touch no other member and are not on the ground: (free, all ends).
pub fn free_ends(members: &[Member], tol: f64) -> (usize, usize) {
    let mem: Vec<&Member> = members.iter().filter(|m| m.kind != "truss").collect();
    let mut ix = Cells::new(1.0_f64.max(tol));
    for (i, m) in mem.iter().enumerate() {
        for p in sample(m.a, m.b, 0.5) {
            ix.add(p, i);
        }
    }
    let zmin = mem.iter().flat_map(|m| [m.a[2], m.b[2]]).fold(f64::MAX, f64::min);
    let mut free = 0;
    for (i, m) in mem.iter().enumerate() {
        for p in [m.a, m.b] {
            if p[2] <= zmin + 0.5 || ix.near(p, tol).iter().any(|&j| j != i) {
                continue;
            }
            free += 1;
        }
    }
    (free, mem.len() * 2)
}

/// How far a shell bends along each axis, relative to its span: second difference of the mean
/// height over a 5 x 5 grid of the plan, the worst row and the worst column.
pub fn double_curve(f: &Facts) -> Option<(f64, f64)> {
    const N: usize = 5;
    let pts: Vec<P3> = if f.has("gridshell_node") && f.n("gridshell_node") > 0 {
        f.shell_pts.clone()
    } else {
        return None;
    };
    let (x0, x1) = (pts.iter().map(|p| p[0]).fold(f64::MAX, f64::min), pts.iter().map(|p| p[0]).fold(f64::MIN, f64::max));
    let (y0, y1) = (pts.iter().map(|p| p[1]).fold(f64::MAX, f64::min), pts.iter().map(|p| p[1]).fold(f64::MIN, f64::max));
    let span = (x1 - x0).max(y1 - y0);
    if span <= 0.0 {
        return None;
    }
    let mut sum = [[0.0; N]; N];
    let mut cnt = [[0usize; N]; N];
    for p in &pts {
        let i = (((p[0] - x0) / (x1 - x0).max(1e-9) * N as f64) as usize).min(N - 1);
        let j = (((p[1] - y0) / (y1 - y0).max(1e-9) * N as f64) as usize).min(N - 1);
        sum[i][j] += p[2];
        cnt[i][j] += 1;
    }
    let z = |i: usize, j: usize| (cnt[i][j] > 0).then(|| sum[i][j] / cnt[i][j] as f64);
    let (mut dx, mut dy) = (0.0f64, 0.0f64);
    for a in 0..N {
        for b in 1..N - 1 {
            if let (Some(p), Some(q), Some(r)) = (z(b - 1, a), z(b, a), z(b + 1, a)) {
                dx = dx.max((p - 2.0 * q + r).abs());
            }
            if let (Some(p), Some(q), Some(r)) = (z(a, b - 1), z(a, b), z(a, b + 1)) {
                dy = dy.max((p - 2.0 * q + r).abs());
            }
        }
    }
    Some((dx / span, dy / span))
}
