// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Oracles for the large-structure, underground, timber and air-delivery hard cases: a clear
//! span between supports, supports that touch the roof, a roof's reach past its masts, how
//! deep and how stacked rooms sit, where shafts stand, how much of the walls, slabs and frame
//! is timber, and whether the air terminals are fed from an air handler.

use super::case::GeomO;
use super::facts::{dist, rows, rx, sample, Cells, Comp, Facts, Member, Run, P3};
use super::oracle::{in_range, no_capability, num, range_text, result, text, EvalCx, Oracle, OracleResult};
use regex::Regex;
use std::collections::HashSet;

const KINDS: [&str; 8] = ["clear_span", "cantilever_reach", "supports_connected", "room_depth", "room_above", "shaft_spread", "timber_share", "terminals_fed"];

pub fn build(g: &GeomO) -> Option<Box<dyn Oracle>> {
    KINDS.contains(&g.kind.as_str()).then(|| Box::new(Design(g.clone())) as Box<dyn Oracle>)
}

struct Design(GeomO);

/// The spanning structure: every member that is not a column.
fn roof(f: &Facts) -> Vec<&Member> {
    f.members.iter().filter(|m| m.kind != "col").collect()
}

/// Where the structure rests: columns whose top reaches its lowest point, arch springings, and
/// components named like supports. Each with the horizontal point and, for a column, its top.
pub fn supports(f: &Facts, named: &Regex, z_tol: f64) -> Vec<(P3, bool)> {
    let r = roof(f);
    if r.is_empty() {
        return vec![];
    }
    let zmin = r.iter().flat_map(|m| [m.a[2], m.b[2]]).fold(f64::MAX, f64::min);
    let mut out: Vec<(P3, bool)> = f
        .members
        .iter()
        .filter(|m| m.kind == "col")
        .filter_map(|m| {
            let top = if m.a[2] >= m.b[2] { m.a } else { m.b };
            (top[2] >= zmin - z_tol).then_some((top, true))
        })
        .collect();
    out.extend(springings(&r, f, z_tol).into_iter().map(|p| (p, true)));
    out.extend(f.comps.iter().filter(|c| named.is_match(&c.family) || named.is_match(&c.category)).map(|c| (c.at, false)));
    out
}

fn off_segment(p: P3, a: P3, b: P3) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let l2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    let t = if l2 > 0.0 { (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1] + (p[2] - a[2]) * d[2]) / l2).clamp(0.0, 1.0) } else { 0.0 };
    dist(p, [a[0] + d[0] * t, a[1] + d[1] * t, a[2] + d[2] * t])
}

/// Where an arch or a rising rib lands at the foot of the structure: the low end of a sloping
/// member at the lowest point built, with no level member (a chord, a deck) meeting it there.
pub fn springings(r: &[&Member], f: &Facts, z_tol: f64) -> Vec<P3> {
    let level = |m: &Member| (m.b[2] - m.a[2]).abs() < 0.2 * dist(m.a, m.b);
    let foot = f.points.iter().chain(r.iter().flat_map(|m| [&m.a, &m.b])).map(|p| p[2]).fold(f64::MAX, f64::min);
    let mut out = vec![];
    for m in r.iter().filter(|m| !level(m)) {
        let low = if m.a[2] <= m.b[2] { m.a } else { m.b };
        let met = r.iter().any(|n| level(n) && off_segment(low, n.a, n.b) < 0.3);
        if low[2] <= foot + z_tol && !met {
            out.push(low);
        }
    }
    out
}

/// Largest gap between neighbouring supports, over both plan axes: (gap, axis, from, to).
pub fn clear_span(sup: &[(P3, bool)]) -> Option<(f64, usize, f64, f64)> {
    let mut best: Option<(f64, usize, f64, f64)> = None;
    for axis in 0..2 {
        let mut v: Vec<f64> = sup.iter().map(|s| s.0[axis]).collect();
        v.sort_by(f64::total_cmp);
        v.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        for w in v.windows(2) {
            if best.is_none_or(|b| w[1] - w[0] > b.0) {
                best = Some((w[1] - w[0], axis, w[0], w[1]));
            }
        }
    }
    best
}

/// The farthest the structure reaches, in plan, from the nearest support.
pub fn reach(f: &Facts, sup: &[(P3, bool)]) -> Option<f64> {
    if sup.is_empty() {
        return None;
    }
    roof(f)
        .iter()
        .flat_map(|m| [m.a, m.b])
        .map(|p| sup.iter().map(|s| (s.0[0] - p[0]).hypot(s.0[1] - p[1])).fold(f64::MAX, f64::min))
        .fold(None, |a: Option<f64>, d| Some(a.map_or(d, |x| x.max(d))))
}

/// Supports that touch the structure: (touching, all, index of the first that does not).
pub fn touching(f: &Facts, sup: &[(P3, bool)], tol: f64, plan_reach: f64) -> (usize, usize, Option<usize>) {
    let mut ix = Cells::new(1.0_f64.max(tol));
    let r = roof(f);
    for (i, m) in r.iter().enumerate() {
        for p in sample(m.a, m.b, 0.5) {
            ix.add(p, i);
        }
    }
    let ends: Vec<P3> = r.iter().flat_map(|m| [m.a, m.b]).collect();
    let mut hit = 0;
    let mut first = None;
    for (i, (p, col)) in sup.iter().enumerate() {
        let ok = if *col { !ix.near(*p, tol).is_empty() } else { ends.iter().any(|e| (e[0] - p[0]).hypot(e[1] - p[1]) <= plan_reach) };
        if ok {
            hit += 1;
        } else if first.is_none() {
            first = Some(i);
        }
    }
    (hit, sup.len(), first)
}

/// Area-weighted timber shares: (walls, slabs) as (timber area, all area).
pub fn timber(m: &ddc_model::Model, mat: &Regex) -> Result<((f64, f64), (f64, f64)), String> {
    let db = m.sql();
    let types = |layers: &str, key: &str| -> Result<HashSet<i64>, String> {
        Ok(rows(db, &format!("SELECT {key}, material FROM {layers}"), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .into_iter()
            .filter(|(_, mt)| mat.is_match(mt))
            .map(|(t, _)| t)
            .collect())
    };
    let wood_w = types("wall_type_layer", "wtype")?;
    let wood_s = types("slab_type_layer", "stype")?;
    let mut w = (0.0, 0.0);
    for (t, ax, ay, bx, by, h) in rows(db, "SELECT wtype, ax, ay, bx, by, height FROM wall", |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?, r.get::<_, f64>(4)?, r.get::<_, f64>(5)?))
    })? {
        let a = (bx - ax).hypot(by - ay) * h;
        w.1 += a;
        if wood_w.contains(&t) {
            w.0 += a;
        }
    }
    let mut s = (0.0, 0.0);
    for (id, t) in rows(db, "SELECT id, stype FROM slab", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let v = rows(db, &format!("SELECT x, y FROM slab_vertex WHERE slab = {id} AND ring = 0 ORDER BY idx"), |r| Ok([r.get::<_, f64>(0)?, r.get::<_, f64>(1)?]))?;
        let a = (0..v.len()).map(|i| v[i][0] * v[(i + 1) % v.len()][1] - v[(i + 1) % v.len()][0] * v[i][1]).sum::<f64>().abs() / 2.0;
        s.1 += a;
        if wood_s.contains(&t) {
            s.0 += a;
        }
    }
    Ok((w, s))
}

/// Air terminals fed from an air handler: terminals on a run whose connected network reaches a
/// source. Returns (fed, all, name of the first unfed).
pub fn terminals_fed(f: &Facts, term: &Regex, source: &Regex) -> (usize, usize, Option<String>) {
    let terms: Vec<&Comp> = f.comps.iter().filter(|c| term.is_match(&c.family) || term.is_match(&c.category)).collect();
    let srcs: Vec<&Comp> = f.comps.iter().filter(|c| source.is_match(&c.family) || source.is_match(&c.category)).collect();
    let n = f.runs.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut Vec<usize>, a: usize) -> usize {
        let mut r = a;
        while p[r] != r {
            r = p[r];
        }
        let mut x = a;
        while p[x] != r {
            let nx = p[x];
            p[x] = r;
            x = nx;
        }
        r
    }
    let mut ix = Cells::new(0.5);
    for (i, r) in f.runs.iter().enumerate() {
        for w in r.pts.windows(2) {
            for p in sample(w[0], w[1], 0.3) {
                ix.add(p, i);
            }
        }
    }
    let ends = |r: &Run| -> Vec<P3> { [r.pts.first(), r.pts.last()].into_iter().flatten().copied().collect() };
    for (i, r) in f.runs.iter().enumerate() {
        for e in ends(r) {
            for j in ix.near(e, 0.35) {
                if f.runs[j].system == r.system {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    parent[a.max(b)] = a.min(b);
                }
            }
        }
    }
    let mut fed_root: HashSet<usize> = HashSet::new();
    for (i, r) in f.runs.iter().enumerate() {
        if ends(r).iter().any(|e| srcs.iter().any(|s| dist(s.at, *e) <= 3.0)) {
            let root = find(&mut parent, i);
            fed_root.insert(root);
        }
    }
    let (mut ok, mut first) = (0, None);
    for t in &terms {
        let near = ix.near(t.at, 1.5);
        let fed = near.into_iter().any(|j| fed_root.contains(&find(&mut parent, j)));
        if fed {
            ok += 1;
        } else if first.is_none() {
            first = Some(format!("{} {}", t.family, t.id));
        }
    }
    (ok, terms.len(), first)
}

impl Oracle for Design {
    fn name(&self) -> String {
        format!("geom {}", self.0.kind)
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let (name, p, f) = (self.name(), &self.0.p, cx.facts);
        let pat = |k: &str, d: &str| rx(&text(p, k).unwrap_or_else(|| d.into()));
        match self.0.kind.as_str() {
            "clear_span" | "cantilever_reach" | "supports_connected" => {
                let named = match pat("supports", "pier|abutment|column|mast|pylon") {
                    Ok(r) => r,
                    Err(e) => return no_capability(&name, "a valid pattern".into(), &e),
                };
                if roof(f).is_empty() {
                    return result(&name, "a spanning structure".into(), "no beams, braces or trusses".into(), false);
                }
                let sup = supports(f, &named, num(p, "z_tol").unwrap_or(1.5));
                match self.0.kind.as_str() {
                    "clear_span" => {
                        let (target, tol) = (num(p, "target"), num(p, "tol").unwrap_or(0.1));
                        let Some(target) = target else {
                            return no_capability(&name, "a target span".into(), "missing parameter target");
                        };
                        let need = target * (1.0 - tol);
                        match clear_span(&sup) {
                            Some((g, axis, a, b)) => result(
                                &name,
                                format!(">= {need:.1} m between neighbouring supports ({target} m minus {:.0} %)", tol * 100.0),
                                format!("{g:.1} m between supports at {} = {a:.1} and {b:.1}", ["x", "y"][axis]),
                                g >= need,
                            ),
                            None => result(&name, format!(">= {need:.1} m between supports"), format!("{} support(s), a span needs two", sup.len()), false),
                        }
                    }
                    "cantilever_reach" => {
                        let (min, max) = (num(p, "min"), num(p, "max"));
                        match reach(f, &sup) {
                            Some(r) => result(&name, format!("{} m from the nearest support", range_text(min, max)), format!("{r:.1} m"), in_range(r, min, max)),
                            None => result(&name, "supports to measure from".into(), "no support reaches the structure's level".into(), false),
                        }
                    }
                    _ => {
                        let (min, share_min) = (num(p, "min").unwrap_or(2.0), num(p, "share_min").unwrap_or(1.0));
                        let (hit, all, first) = touching(f, &sup, num(p, "tol").unwrap_or(0.5), num(p, "reach").unwrap_or(3.0));
                        let share = if all == 0 { 0.0 } else { hit as f64 / all as f64 };
                        let stray = first.map(|i| format!("; support at ({:.1}, {:.1}) touches nothing", sup[i].0[0], sup[i].0[1])).unwrap_or_default();
                        result(
                            &name,
                            format!(">= {min} supports, >= {share_min} of them touching the structure"),
                            format!("{hit} of {all} supports touch it ({share:.2}){stray}"),
                            all as f64 >= min && share + 1e-9 >= share_min,
                        )
                    }
                }
            }
            "room_depth" => {
                let (Some(room), Some(depth)) = (text(p, "room"), num(p, "depth")) else {
                    return no_capability(&name, "room and depth".into(), "missing parameter room or depth");
                };
                let re = match rx(&room) {
                    Ok(r) => r,
                    Err(e) => return no_capability(&name, "a valid pattern".into(), &e),
                };
                let tol = num(p, "tol").unwrap_or(0.25);
                let zs: Vec<f64> = f.rooms.iter().filter(|r| re.is_match(&r.name)).filter_map(|r| f.level_z.get(&r.level).copied()).collect();
                let want = format!("room z within {:.1}..{:.1} m (ground at 0, {depth} m +/- {:.0} %)", -depth * (1.0 + tol), -depth * (1.0 - tol), tol * 100.0);
                if zs.is_empty() {
                    return result(&name, want, format!("no room named like {room}"), false);
                }
                let (lo, hi) = (zs.iter().cloned().fold(f64::MAX, f64::min), zs.iter().cloned().fold(f64::MIN, f64::max));
                result(&name, want, format!("{} room(s) at z {lo:.1}..{hi:.1} m", zs.len()), lo >= -depth * (1.0 + tol) - 1e-9 && hi <= -depth * (1.0 - tol) + 1e-9)
            }
            "room_above" => {
                let (Some(upper), Some(lower)) = (text(p, "upper"), text(p, "lower")) else {
                    return no_capability(&name, "upper and lower patterns".into(), "missing parameter upper or lower");
                };
                let (u, l) = match (rx(&upper), rx(&lower)) {
                    (Ok(a), Ok(b)) => (a, b),
                    (Err(e), _) | (_, Err(e)) => return no_capability(&name, "valid patterns".into(), &e),
                };
                let gap = num(p, "gap_min").unwrap_or(3.0);
                let z = |re: &Regex| -> Vec<f64> { f.rooms.iter().filter(|r| re.is_match(&r.name)).filter_map(|r| f.level_z.get(&r.level).copied()).collect() };
                let (uz, lz) = (z(&u), z(&l));
                if uz.is_empty() || lz.is_empty() {
                    return result(&name, format!("{upper} stacked {gap} m above {lower}"), format!("{} {upper} room(s), {} {lower} room(s)", uz.len(), lz.len()), false);
                }
                let d = uz.iter().cloned().fold(f64::MAX, f64::min) - lz.iter().cloned().fold(f64::MIN, f64::max);
                result(&name, format!("{upper} at least {gap} m above {lower}"), format!("lowest {upper} is {d:.1} m above the highest {lower}"), d >= gap - 1e-9)
            }
            "shaft_spread" => {
                let (count_min, apart_min) = (num(p, "count_min").unwrap_or(2.0), num(p, "apart_min").unwrap_or(60.0));
                let c: Vec<[f64; 2]> = f.shafts.iter().filter_map(|s| s.bbox).map(|b| [(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0]).collect();
                let apart = c.iter().flat_map(|a| c.iter().map(move |b| (a[0] - b[0]).hypot(a[1] - b[1]))).fold(0.0, f64::max);
                result(
                    &name,
                    format!(">= {count_min} shafts, two of them >= {apart_min} m apart"),
                    format!("{} shaft(s) with a footprint, farthest pair {apart:.1} m apart", c.len()),
                    c.len() as f64 >= count_min && apart >= apart_min,
                )
            }
            "timber_share" => {
                let mat = match rx(&text(p, "material").unwrap_or_else(|| "clt|cross.?lam|glulam|glue.?lam|timber|wood|lvl|brettsperr|massivholz".into())) {
                    Ok(r) => r,
                    Err(e) => return no_capability(&name, "a valid pattern".into(), &e),
                };
                let (walls_min, slabs_min, frame_min) = (num(p, "walls_min").unwrap_or(0.4), num(p, "slabs_min").unwrap_or(0.8), num(p, "frame_min").unwrap_or(0.8));
                let ((w, wt), (s, st)) = match timber(&cx.ed.model, &mat) {
                    Ok(x) => x,
                    Err(e) => return no_capability(&name, "wall and slab tables".into(), &format!("no capability: {e}")),
                };
                let share = |a: f64, b: f64| if b <= 0.0 { 0.0 } else { a / b };
                let frame: Vec<&Member> = f.members.iter().filter(|m| m.kind == "col" || m.kind == "beam").collect();
                let wood = frame.iter().filter(|m| m.mtype.and_then(|t| f.mtypes.get(&t)).is_some_and(|t| mat.is_match(&t.2))).count();
                let (ws, ss, fs) = (share(w, wt), share(s, st), share(wood as f64, frame.len() as f64));
                result(
                    &name,
                    format!("timber walls >= {walls_min}, slabs >= {slabs_min}, columns and beams >= {frame_min}"),
                    format!("walls {ws:.2} of {wt:.0} m2, slabs {ss:.2} of {st:.0} m2, frame {fs:.2} of {}", frame.len()),
                    wt > 0.0 && st > 0.0 && !frame.is_empty() && ws + 1e-9 >= walls_min && ss + 1e-9 >= slabs_min && fs + 1e-9 >= frame_min,
                )
            }
            "terminals_fed" => {
                let (term, src) = match (pat("terminal", "air_terminal|diffuser|grille"), pat("source", "ahu|air.?handling|fan.?coil|fan")) {
                    (Ok(a), Ok(b)) => (a, b),
                    (Err(e), _) | (_, Err(e)) => return no_capability(&name, "valid patterns".into(), &e),
                };
                let share_min = num(p, "share_min").unwrap_or(0.95);
                let (ok, all, first) = terminals_fed(f, &term, &src);
                if all == 0 {
                    return result(&name, format!(">= {share_min} of terminals fed from an air handler"), "no air terminals".into(), false);
                }
                let share = ok as f64 / all as f64;
                let tail = first.map(|t| format!("; unfed: {t}")).unwrap_or_default();
                result(&name, format!(">= {share_min} of terminals fed from an air handler"), format!("{ok} of {all} terminals fed ({share:.2}){tail}"), share + 1e-9 >= share_min)
            }
            _ => unreachable!("build admits only known kinds"),
        }
    }
}
