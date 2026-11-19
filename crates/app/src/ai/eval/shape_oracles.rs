// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Oracles for what makes a hard case that case: a tower that reaches its height, is braced and
//! hangs together; a plant whose pipes join nozzles; a data hall with contained aisles; a shell
//! that curves both ways.

use super::case::GeomO;
use super::facts::{rx, sample, Cells, Comp, Facts, Member, P3};
use super::geom_oracles::height;
use super::oracle::{in_range, no_capability, num, result, text, EvalCx, Oracle, OracleResult};
use std::collections::BTreeMap;

pub fn build(g: &GeomO) -> Option<Box<dyn Oracle>> {
    ["height_target", "members_connected", "bracing", "equipment_by_kind", "pipe_nozzles", "aisle_containment", "gaussian_curve", "saddle_curve", "sweep_law"]
        .contains(&g.kind.as_str())
        .then(|| Box::new(Shape(g.clone())) as Box<dyn Oracle>)
}

struct Shape(GeomO);

struct Dsu(Vec<usize>);

impl Dsu {
    fn find(&mut self, a: usize) -> usize {
        let mut r = a;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut x = a;
        while self.0[x] != r {
            let n = self.0[x];
            self.0[x] = r;
            x = n;
        }
        r
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

/// Components of the structural members joined where an end meets another member: (count,
/// index of a member in the smallest one when there is more than one component).
pub fn member_parts(members: &[Member], bases: &[P3], tol: f64) -> (usize, Option<usize>) {
    let mem: Vec<(usize, &Member)> = members.iter().enumerate().filter(|(_, m)| m.kind != "truss").collect();
    if mem.is_empty() {
        return (0, None);
    }
    // bases and platforms are nodes of their own after the members; one is joined to a member
    // passing within 1.5 m of it
    let total = mem.len() + bases.len();
    let mut ix = Cells::new(1.0_f64.max(tol));
    for (i, (_, m)) in mem.iter().enumerate() {
        for p in sample(m.a, m.b, 0.5) {
            ix.add(p, i);
        }
    }
    let mut d = Dsu((0..total).collect());
    for (i, (_, m)) in mem.iter().enumerate() {
        for p in [m.a, m.b] {
            for j in ix.near(p, tol) {
                d.union(i, j);
            }
        }
    }
    for (k, b) in bases.iter().enumerate() {
        for j in ix.near(*b, 1.5) {
            d.union(mem.len() + k, j);
        }
    }
    // first: index into members, or members.len() + base index for a part made of a base alone
    let first_of = |i: usize| if i < mem.len() { mem[i].0 } else { members.len() + i - mem.len() };
    let mut size: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    for i in 0..total {
        let r = d.find(i);
        let e = size.entry(r).or_insert((0, first_of(i)));
        e.0 += 1;
    }
    let small = size.values().min_by_key(|(n, first)| (*n, *first)).map(|(_, f)| *f);
    (size.len(), (size.len() > 1).then_some(small).flatten())
}

/// Diagonal braces per (z band, face sector): how many cells hold any, how many hold both
/// leans (an X or a zig-zag), as seen from outside the tower.
pub fn bracing(members: &[Member], bands: usize) -> (usize, usize, usize) {
    let all: Vec<P3> = members.iter().flat_map(|m| [m.a, m.b]).collect();
    if all.is_empty() || bands == 0 {
        return (0, 0, 0);
    }
    let n = all.len() as f64;
    let (cx, cy) = (all.iter().map(|p| p[0]).sum::<f64>() / n, all.iter().map(|p| p[1]).sum::<f64>() / n);
    let (z0, z1) = (all.iter().map(|p| p[2]).fold(f64::MAX, f64::min), all.iter().map(|p| p[2]).fold(f64::MIN, f64::max));
    let mut cells: BTreeMap<(usize, usize), (bool, bool)> = BTreeMap::new();
    let mut diagonals = 0;
    for m in members.iter().filter(|m| m.kind == "brace") {
        let (lo, hi) = if m.a[2] <= m.b[2] { (m.a, m.b) } else { (m.b, m.a) };
        let (dx, dy, dz) = (hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]);
        let h = dx.hypot(dy);
        // between about 15 and 75 degrees from the horizontal
        if dz < 0.26 * h || h < 0.26 * dz {
            continue;
        }
        diagonals += 1;
        let (mx, my, mz) = ((lo[0] + hi[0]) / 2.0 - cx, (lo[1] + hi[1]) / 2.0 - cy, (lo[2] + hi[2]) / 2.0);
        let r = mx.hypot(my).max(1e-9);
        let lean = dx * (-my / r) + dy * (mx / r);
        let band = (((mz - z0) / (z1 - z0).max(1e-9) * bands as f64) as usize).min(bands - 1);
        let sector = ((my.atan2(mx) + std::f64::consts::PI) / (std::f64::consts::PI / 4.0)) as usize % 8;
        let c = cells.entry((band, sector)).or_default();
        if lean >= 0.0 {
            c.0 = true;
        } else {
            c.1 = true;
        }
    }
    (diagonals, cells.len(), cells.values().filter(|c| c.0 && c.1).count())
}

fn parse_kinds(s: &str) -> Result<Vec<(String, usize)>, String> {
    s.split(';')
        .filter(|x| !x.trim().is_empty())
        .map(|k| {
            let (pat, n) = k.rsplit_once(':').ok_or(format!("kind {k:?} needs :count"))?;
            Ok((pat.trim().to_string(), n.trim().parse::<usize>().map_err(|_| format!("count of {k:?}"))?))
        })
        .collect()
}

/// Pipe lines (runs of domain pipe joined end to end): each with the number of its loose ends
/// that sit on a connector: a world port of a family, or a component named like a nozzle. An end counts once
/// however many connectors stand near it; a point where lines join is not an end.
pub fn pipe_lines(f: &Facts, nozzle: &regex::Regex) -> Vec<(i64, usize)> {
    let runs: Vec<_> = f.runs.iter().filter(|r| r.domain == "pipe" && r.pts.len() >= 2).collect();
    let mut ix = Cells::new(0.5);
    for (i, r) in runs.iter().enumerate() {
        for w in r.pts.windows(2) {
            for p in sample(w[0], w[1], 0.3) {
                ix.add(p, i);
            }
        }
    }
    let mut d = Dsu((0..runs.len()).collect());
    let mut loose: Vec<Vec<P3>> = vec![vec![]; runs.len()];
    for (i, r) in runs.iter().enumerate() {
        for p in [r.pts[0], r.pts[r.pts.len() - 1]] {
            let others: Vec<usize> = ix.near(p, 0.15).into_iter().filter(|j| *j != i).collect();
            for j in &others {
                d.union(i, *j);
            }
            if others.is_empty() {
                loose[i].push(p);
            }
        }
    }
    // a connector is where it faces the pipe: its world port; a component named like a nozzle
    // is its own connector
    let mut seats: Vec<P3> = f.ports.clone();
    seats.extend(f.comps.iter().filter(|c| nozzle.is_match(&c.family) || nozzle.is_match(&c.category)).map(|c| c.at));
    let mut lines: BTreeMap<usize, (i64, Vec<P3>)> = BTreeMap::new();
    for (i, r) in runs.iter().enumerate() {
        let root = d.find(i);
        let e = lines.entry(root).or_insert((r.id, vec![]));
        e.0 = e.0.min(r.id);
        e.1.extend(loose[i].iter().copied());
    }
    lines
        .into_values()
        .map(|(id, ends)| (id, ends.iter().filter(|e| seats.iter().any(|s| super::facts::dist(*s, **e) <= 0.6)).count()))
        .collect()
}

/// Rows of racks along one axis and the pitches between them on the level with most racks:
/// (rows, pitches in order).
pub fn rack_rows(racks: &[&Comp], row_min: usize) -> (usize, Vec<f64>) {
    let mut best = (0usize, vec![]);
    for axis in [1usize, 0] {
        let mut by_level: BTreeMap<i64, BTreeMap<i64, usize>> = BTreeMap::new();
        for c in racks {
            let at = [c.at[0], c.at[1]];
            *by_level.entry(c.level).or_default().entry((at[axis] / 0.3).round() as i64).or_default() += 1;
        }
        for rows in by_level.values() {
            // neighbouring buckets of one row are the same row
            let mut keys: Vec<(f64, usize)> = vec![];
            for (k, n) in rows {
                match keys.last_mut() {
                    Some(last) if (*k as f64 * 0.3 - last.0).abs() <= 0.31 => last.1 += n,
                    _ => keys.push((*k as f64 * 0.3, *n)),
                }
            }
            let rows: Vec<f64> = keys.iter().filter(|(_, n)| *n >= row_min).map(|(y, _)| *y).collect();
            if rows.len() > best.0 {
                best = (rows.len(), rows.windows(2).map(|w| w[1] - w[0]).collect());
            }
        }
    }
    best
}

/// Pitches alternate short, long, short ... : the rows stand back to back with an aisle between
/// pairs. `Some(aisles)` when they do.
pub fn alternating(pitches: &[f64]) -> Option<usize> {
    let (lo, hi) = (pitches.iter().cloned().fold(f64::MAX, f64::min), pitches.iter().cloned().fold(f64::MIN, f64::max));
    if pitches.len() < 2 || hi < 1.5 * lo {
        return None;
    }
    let mid = (lo + hi) / 2.0;
    let long: Vec<bool> = pitches.iter().map(|p| *p > mid).collect();
    long.windows(2).all(|w| w[0] != w[1]).then(|| long.iter().filter(|l| **l).count())
}

/// Gaussian curvature times span squared on a coarse grid of mean heights: how many interior
/// cells are curved, of how many.
pub fn gaussian(pts: &[P3]) -> Option<(usize, usize, f64)> {
    let k = gauss_k(pts)?;
    Some((k.len(), k.iter().filter(|k| k.abs() > 0.05).count(), k.iter().fold(0.0f64, |a, k| a.max(k.abs()))))
}

/// Signed Gaussian curvature (scaled by the span squared) at each interior cell of a 7 x 7 grid.
pub fn gauss_k(pts: &[P3]) -> Option<Vec<f64>> {
    const N: usize = 7;
    let (x0, x1) = (pts.iter().map(|p| p[0]).fold(f64::MAX, f64::min), pts.iter().map(|p| p[0]).fold(f64::MIN, f64::max));
    let (y0, y1) = (pts.iter().map(|p| p[1]).fold(f64::MAX, f64::min), pts.iter().map(|p| p[1]).fold(f64::MIN, f64::max));
    let span = (x1 - x0).max(y1 - y0);
    if pts.len() < 20 || span <= 0.0 {
        return None;
    }
    let (mut sum, mut cnt) = ([[0.0; N]; N], [[0usize; N]; N]);
    for p in pts {
        let i = (((p[0] - x0) / (x1 - x0).max(1e-9) * N as f64) as usize).min(N - 1);
        let j = (((p[1] - y0) / (y1 - y0).max(1e-9) * N as f64) as usize).min(N - 1);
        sum[i][j] += p[2];
        cnt[i][j] += 1;
    }
    let z = |i: usize, j: usize| (cnt[i][j] > 0).then(|| sum[i][j] / cnt[i][j] as f64);
    let (hx, hy) = ((x1 - x0) / N as f64, (y1 - y0) / N as f64);
    let mut ks = vec![];
    for i in 1..N - 1 {
        for j in 1..N - 1 {
            let (Some(c), Some(e), Some(w), Some(n), Some(s), Some(ne), Some(nw), Some(se), Some(sw)) =
                (z(i, j), z(i + 1, j), z(i - 1, j), z(i, j + 1), z(i, j - 1), z(i + 1, j + 1), z(i - 1, j + 1), z(i + 1, j - 1), z(i - 1, j - 1))
            else {
                continue;
            };
            let (zx, zy) = ((e - w) / (2.0 * hx), (n - s) / (2.0 * hy));
            let (zxx, zyy) = ((e - 2.0 * c + w) / (hx * hx), (n - 2.0 * c + s) / (hy * hy));
            let zxy = (ne - nw - se + sw) / (4.0 * hx * hy);
            let k = (zxx * zyy - zxy * zxy) / (1.0 + zx * zx + zy * zy).powi(2) * span * span;
            ks.push(k);
        }
    }
    (!ks.is_empty()).then_some(ks)
}

/// Saddle: share of cells with negative curvature and with positive. A thin net (a hypar's
/// control corners) has too few points to grid; then the four corner points must not be
/// coplanar, and the warp is half the difference of the diagonal height sums.
pub fn saddle(pts: &[P3]) -> Option<(String, f64, f64)> {
    if pts.len() >= 20 {
        let k = gauss_k(pts)?;
        let (neg, pos) = (k.iter().filter(|k| **k < -0.05).count() as f64, k.iter().filter(|k| **k > 0.05).count() as f64);
        let n = k.len() as f64;
        return Some((format!("{neg:.0} of {n:.0} samples saddle-shaped, {pos:.0} dome or bowl"), neg / n, pos / n));
    }
    if pts.len() < 4 {
        return None;
    }
    let (x0, x1) = (pts.iter().map(|p| p[0]).fold(f64::MAX, f64::min), pts.iter().map(|p| p[0]).fold(f64::MIN, f64::max));
    let (y0, y1) = (pts.iter().map(|p| p[1]).fold(f64::MAX, f64::min), pts.iter().map(|p| p[1]).fold(f64::MIN, f64::max));
    let at = |x: f64, y: f64| pts.iter().min_by(|a, b| ((a[0] - x).hypot(a[1] - y)).total_cmp(&(b[0] - x).hypot(b[1] - y))).map(|p| p[2]);
    let (a, b, c, d) = (at(x0, y0)?, at(x1, y1)?, at(x1, y0)?, at(x0, y1)?);
    let warp = ((a + b) - (c + d)).abs() / 2.0;
    Some((format!("corner warp {warp:.2} m"), warp, 0.0))
}

impl Oracle for Shape {
    fn name(&self) -> String {
        format!("geom {}", self.0.kind)
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let (name, p, f) = (self.name(), &self.0.p, cx.facts);
        match self.0.kind.as_str() {
            "height_target" => {
                let (Some(target), tol) = (num(p, "target"), num(p, "tol").unwrap_or(0.02)) else {
                    return no_capability(&name, "a target height".into(), "missing parameter target");
                };
                let need = target * (1.0 - tol);
                match height(f) {
                    Some(h) => result(&name, format!(">= {need:.1} m ({target} m minus {:.0} %)", tol * 100.0), format!("{h:.2} m"), h >= need),
                    None => result(&name, format!(">= {need:.1} m"), "nothing built to measure".into(), false),
                }
            }
            "members_connected" => {
                let base_re = match rx(&text(p, "bases").unwrap_or_else(|| "platform|base.?plate|footing|pad|landing".into())) {
                    Ok(r) => r,
                    Err(e) => return no_capability(&name, "a valid pattern".into(), &e),
                };
                let mut bases = f.bases.clone();
                bases.extend(f.comps.iter().filter(|c| base_re.is_match(&c.family) || base_re.is_match(&c.category)).map(|c| c.at));
                let (n, stray) = member_parts(&f.members, &bases, num(p, "tol").unwrap_or(0.3));
                let max = num(p, "max").unwrap_or(1.0);
                let shown = match (n, stray) {
                    (0, _) => "no members".to_string(),
                    (_, Some(i)) if i >= f.members.len() => format!("{n} separate parts, base {} touches no member", i - f.members.len()),
                    (_, Some(i)) => format!("{n} separate parts, smallest holds member {i}"),
                    _ => format!("{n} part"),
                };
                result(&name, format!("1..={max} parts"), shown, n >= 1 && n as f64 <= max)
            }
            "bracing" => {
                let bands = num(p, "bands").unwrap_or(6.0);
                let (diag, cells, both) = bracing(&f.members, bands as usize);
                let (share_min, diag_min) = (num(p, "share_min").unwrap_or(0.5), num(p, "diagonals_min").unwrap_or(8.0));
                let cells_min = num(p, "cells_min").unwrap_or(bands * 4.0);
                let share = if cells == 0 { 0.0 } else { both as f64 / cells as f64 };
                result(
                    &name,
                    format!(">= {diag_min} diagonals on >= {cells_min} faces, >= {share_min} of braced faces crossed or zig-zag"),
                    format!("{diag} diagonals, {both} of {cells} braced faces cross ({share:.2})"),
                    diag as f64 >= diag_min && cells as f64 >= cells_min && share + 1e-9 >= share_min,
                )
            }
            "equipment_by_kind" => {
                let kinds = match parse_kinds(&text(p, "kinds").unwrap_or_default()).and_then(|k| if k.is_empty() { Err("no kinds".into()) } else { Ok(k) }) {
                    Ok(k) => k,
                    Err(e) => return no_capability(&name, "kinds like \"vessel|tank:3;pump:2\"".into(), &e),
                };
                let mut shown = vec![];
                let mut ok = true;
                for (pat, min) in &kinds {
                    let re = match rx(pat) {
                        Ok(r) => r,
                        Err(e) => return no_capability(&name, "valid patterns".into(), &e),
                    };
                    let n = f.comps.iter().filter(|c| re.is_match(&c.family) || re.is_match(&c.category)).count();
                    ok &= n >= *min;
                    shown.push(format!("{pat} {n}/{min}"));
                }
                result(&name, "every kind at its minimum".into(), shown.join(", "), ok)
            }
            "pipe_nozzles" => {
                let re = match rx(&text(p, "nozzle").unwrap_or_else(|| "nozzle|flange".into())) {
                    Ok(r) => r,
                    Err(e) => return no_capability(&name, "a valid pattern".into(), &e),
                };
                let share_min = num(p, "share_min").unwrap_or(1.0);
                let lines = pipe_lines(f, &re);
                if lines.is_empty() {
                    return result(&name, format!(">= {share_min} of pipe lines join two nozzles"), "no pipe runs".into(), false);
                }
                let good = lines.iter().filter(|(_, n)| *n >= 2).count();
                let share = good as f64 / lines.len() as f64;
                let bad = lines.iter().find(|(_, n)| *n < 2).map(|(id, n)| format!("; line of run {id} reaches {n} nozzle(s)")).unwrap_or_default();
                result(
                    &name,
                    format!(">= {share_min} of pipe lines join two nozzles"),
                    format!("{good} of {} pipe lines join two nozzles ({share:.2}){bad}", lines.len()),
                    share + 1e-9 >= share_min,
                )
            }
            "aisle_containment" => {
                let (racks_re, cont_re) = match (
                    rx(&text(p, "racks").unwrap_or_else(|| "rack|cabinet".into())),
                    rx(&text(p, "containment").unwrap_or_else(|| "containment|aisle|curtain|blanking|roof panel".into())),
                ) {
                    (Ok(a), Ok(b)) => (a, b),
                    (Err(e), _) | (_, Err(e)) => return no_capability(&name, "valid patterns".into(), &e),
                };
                let racks: Vec<&Comp> = f.comps.iter().filter(|c| racks_re.is_match(&c.family) || racks_re.is_match(&c.category)).collect();
                let cont = f.comps.iter().filter(|c| cont_re.is_match(&c.family) || cont_re.is_match(&c.category)).count();
                let (aisles_min, cont_min) = (num(p, "aisles_min").unwrap_or(2.0), num(p, "containment_min").unwrap_or(1.0));
                let (rows, pitches) = rack_rows(&racks, num(p, "row_min").unwrap_or(3.0) as usize);
                let alt = alternating(&pitches);
                let shown = format!(
                    "{} racks in {rows} rows, {}, {cont} containment element(s)",
                    racks.len(),
                    match alt {
                        Some(a) => format!("{a} aisle(s) between back-to-back pairs"),
                        None => "rows do not alternate back to back and aisle".to_string(),
                    }
                );
                let ok = alt.is_some_and(|a| a as f64 >= aisles_min) && cont as f64 >= cont_min;
                result(&name, format!(">= {aisles_min} aisles, >= {cont_min} containment elements"), shown, ok)
            }
            "gaussian_curve" => {
                if !(f.has("gridshell_node") && f.n("gridshell_node") > 0) {
                    return no_capability(&name, "a shell with nodes".into(), "no capability: the model has no gridshell nodes");
                }
                let share_min = num(p, "share_min").unwrap_or(0.3);
                match gaussian(&f.shell_pts) {
                    Some((cells, curved, peak)) => {
                        let share = curved as f64 / cells as f64;
                        result(
                            &name,
                            format!("Gaussian curvature not zero on >= {share_min} of samples"),
                            format!("{curved} of {cells} samples curved ({share:.2}), peak {peak:.2}"),
                            in_range(share, Some(share_min), None),
                        )
                    }
                    None => result(&name, "enough nodes to sample".into(), "too few nodes to sample".into(), false),
                }
            }
            "sweep_law" => {
                let (turns_min, scale_min, rise_min) = (num(p, "turns_min").unwrap_or(1.0), num(p, "scale_min").unwrap_or(2.0), num(p, "rise_min").unwrap_or(0.0));
                if !f.has("solid") {
                    return no_capability(&name, "swept solids".into(), "no capability: the model has no solid table");
                }
                let turns = |s: &super::facts::SweepFact| s.twist.max(s.turn) / (2.0 * std::f64::consts::PI);
                let want = format!("a sweep turning >= {turns_min} turns, scaling >= {scale_min} x, rising >= {rise_min} m");
                let ok = f.sweeps.iter().find(|s| turns(s) + 1e-9 >= turns_min && s.scale + 1e-9 >= scale_min && s.rise + 1e-9 >= rise_min);
                let best = f.sweeps.iter().max_by(|a, b| (turns(a) * a.scale).total_cmp(&(turns(b) * b.scale)));
                match (ok, best) {
                    (Some(s), _) | (None, Some(s)) => result(
                        &name,
                        want,
                        format!("sweep {}: {:.2} turns, scale x{:.2}, rise {:.1} m", s.id, turns(s), s.scale, s.rise),
                        ok.is_some(),
                    ),
                    _ => result(&name, want, "no swept solid".into(), false),
                }
            }
            "saddle_curve" => {
                if f.shell_pts.is_empty() {
                    return no_capability(&name, "a shell with nodes or a control net".into(), "no capability: the model has no gridshell nodes or freeform net");
                }
                let (share_min, warp_min) = (num(p, "share_min").unwrap_or(0.5), num(p, "warp_min").unwrap_or(2.0));
                match saddle(&f.shell_pts) {
                    Some((shown, a, b)) if f.shell_pts.len() >= 20 => {
                        result(&name, format!("saddle (negative Gaussian curvature) on >= {share_min} of samples, and more than the dome share"), format!("{shown} ({a:.2})"), a + 1e-9 >= share_min && a > b)
                    }
                    Some((shown, warp, _)) => result(&name, format!("four corners not coplanar, warp >= {warp_min} m"), shown, warp + 1e-9 >= warp_min),
                    None => result(&name, "enough points to sample".into(), "too few points to sample".into(), false),
                }
            }
            _ => unreachable!("build admits only known kinds"),
        }
    }
}
