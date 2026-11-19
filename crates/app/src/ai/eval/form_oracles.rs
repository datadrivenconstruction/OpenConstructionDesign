//! Oracles for whether a free form reads as its shape, not only as a count of parts: a twisted
//! tower turns every storey by its angle, a hyperboloid narrows to a waist above its base and
//! flares again, a dome or a geodesic sphere keeps its nodes on one sphere of the asked radius,
//! a stepped pyramid sets every storey back by its step, a tapered tower narrows to its top.

use super::case::GeomO;
use super::facts::{rows, P3};
use super::oracle::{in_range, no_capability, num, result, EvalCx, Oracle, OracleResult};
use std::collections::HashMap;

const KINDS: [&str; 5] = ["twist_per_floor", "waist", "sphere_fit", "setback_per_floor", "top_ratio"];

pub fn build(g: &GeomO) -> Option<Box<dyn Oracle>> {
    KINDS.contains(&g.kind.as_str()).then(|| Box::new(Form(g.clone())) as Box<dyn Oracle>)
}

struct Form(GeomO);

/// Orientation of an outline in degrees, (-45, 45]: the length-weighted mean of its edge
/// directions taken with period 90, so a square or a rectangle reads the same whichever edge
/// comes first. None when the edges cancel (a circle or a regular octagon has no heading).
pub fn heading(ring: &[[f64; 2]]) -> Option<f64> {
    let (mut s, mut c) = (0.0, 0.0);
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let t = 4.0 * dy.atan2(dx);
        s += dx.hypot(dy) * t.sin();
        c += dx.hypot(dy) * t.cos();
    }
    (s.hypot(c) > 1e-6 * ring.len() as f64).then(|| s.atan2(c).to_degrees() / 4.0)
}

/// The turn from heading `a` to heading `b`, wrapped to (-45, 45].
pub fn turn(a: f64, b: f64) -> f64 {
    let d = (b - a + 45.0).rem_euclid(90.0) - 45.0;
    if d <= -45.0 { d + 90.0 } else { d }
}

fn area(r: &[[f64; 2]]) -> f64 {
    (0..r.len()).map(|i| r[i][0] * r[(i + 1) % r.len()][1] - r[(i + 1) % r.len()][0] * r[i][1]).sum::<f64>().abs() / 2.0
}

/// The largest slab outline of each level, ordered by height.
pub fn storey_outlines(m: &ddc_model::Model) -> Result<Vec<(f64, Vec<[f64; 2]>)>, String> {
    let db = m.sql();
    let lz: HashMap<i64, f64> = rows(db, "SELECT id, elevation FROM level", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))?.into_iter().collect();
    let mut best: HashMap<i64, (f64, f64, Vec<[f64; 2]>)> = HashMap::new();
    for (id, level, off) in rows(db, "SELECT id, level, offset FROM slab", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?)))? {
        let v = rows(db, &format!("SELECT x, y FROM slab_vertex WHERE slab = {id} AND ring = 0 ORDER BY idx"), |r| Ok([r.get::<_, f64>(0)?, r.get::<_, f64>(1)?]))?;
        let a = area(&v);
        if v.len() >= 3 && best.get(&level).is_none_or(|b| a > b.1) {
            best.insert(level, (lz.get(&level).copied().unwrap_or(0.0) + off, a, v));
        }
    }
    let mut out: Vec<(f64, Vec<[f64; 2]>)> = best.into_values().map(|(z, _, v)| (z, v)).collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(out)
}

/// Points of the tower alone and its vertical axis: the core is every element reaching above
/// half the height; the tower adds the elements lying wholly within 1.4 core radii of the
/// core's axis (a keeper's house beside the foot stays out); the axis is the centre of the
/// tower's base footprint.
pub fn tower_points(elems: &[Vec<P3>]) -> Option<(Vec<P3>, [f64; 2])> {
    let all = elems.iter().flatten();
    let z0 = all.clone().map(|p| p[2]).fold(f64::MAX, f64::min);
    let h = all.map(|p| p[2]).fold(f64::MIN, f64::max) - z0;
    if !(h > 0.0) {
        return None;
    }
    fn centre<'a>(pts: impl Iterator<Item = &'a P3>) -> [f64; 2] {
        let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in pts {
            (x0, x1, y0, y1) = (x0.min(p[0]), x1.max(p[0]), y0.min(p[1]), y1.max(p[1]));
        }
        [(x0 + x1) / 2.0, (y0 + y1) / 2.0]
    }
    let core: Vec<&Vec<P3>> = elems.iter().filter(|e| e.iter().any(|p| p[2] >= z0 + 0.5 * h)).collect();
    let c = centre(core.iter().flat_map(|e| e.iter()));
    let off = |p: &P3, c: [f64; 2]| (p[0] - c[0]).hypot(p[1] - c[1]);
    let reach = 1.4 * core.iter().flat_map(|e| e.iter()).map(|p| off(p, c)).fold(0.0, f64::max);
    let tower: Vec<P3> = elems.iter().filter(|e| e.iter().all(|p| off(p, c) <= reach + 1e-9)).flatten().copied().collect();
    let foot = tower.iter().map(|p| p[2]).fold(f64::MAX, f64::min);
    let axis = centre(tower.iter().filter(|p| p[2] <= foot + 0.1 * h));
    Some((tower, axis))
}

fn perimeter(r: &[[f64; 2]]) -> f64 {
    (0..r.len()).map(|i| (r[(i + 1) % r.len()][0] - r[i][0]).hypot(r[(i + 1) % r.len()][1] - r[i][1])).sum()
}

/// Storey-to-storey set-backs, metres, bottom up: the area lost over the mean perimeter, which
/// is the inset of an outline offset inwards (exact for a square or a rectangle).
pub fn storey_setbacks(outlines: &[(f64, Vec<[f64; 2]>)]) -> Vec<f64> {
    outlines
        .windows(2)
        .map(|w| {
            let (a, b) = (&w[0].1, &w[1].1);
            (area(a) - area(b)) / ((perimeter(a) + perimeter(b)) / 2.0).max(1e-9)
        })
        .collect()
}

/// Storey-to-storey turns of the outlines, degrees, bottom up (storeys with no heading skipped).
pub fn storey_turns(outlines: &[(f64, Vec<[f64; 2]>)]) -> Vec<f64> {
    let h: Vec<f64> = outlines.iter().filter_map(|(_, r)| heading(r)).collect();
    h.windows(2).map(|w| turn(w[0], w[1])).collect()
}

/// Outer radius of the structure in `bands` height bands about the plan centre of its bounds:
/// (band middle z, radius) of each band that holds a point.
pub fn radii(pts: &[P3], bands: usize) -> Vec<(f64, f64)> {
    if pts.is_empty() || bands == 0 {
        return vec![];
    }
    let lo = |i: usize| pts.iter().map(|p| p[i]).fold(f64::MAX, f64::min);
    let hi = |i: usize| pts.iter().map(|p| p[i]).fold(f64::MIN, f64::max);
    radii_about(pts, bands, [(lo(0) + hi(0)) / 2.0, (lo(1) + hi(1)) / 2.0])
}

/// Widest distance from the vertical axis through `c`, per height band (band mid z, radius).
pub fn radii_about(pts: &[P3], bands: usize, [cx, cy]: [f64; 2]) -> Vec<(f64, f64)> {
    if pts.is_empty() || bands == 0 {
        return vec![];
    }
    let lo = |i: usize| pts.iter().map(|p| p[i]).fold(f64::MAX, f64::min);
    let hi = |i: usize| pts.iter().map(|p| p[i]).fold(f64::MIN, f64::max);
    let (z0, h) = (lo(2), hi(2) - lo(2));
    if h <= 0.0 {
        return vec![];
    }
    let mut r = vec![None::<f64>; bands];
    for p in pts {
        let k = (((p[2] - z0) / h * bands as f64) as usize).min(bands - 1);
        let d = (p[0] - cx).hypot(p[1] - cy);
        r[k] = Some(r[k].map_or(d, |m| m.max(d)));
    }
    r.into_iter().enumerate().filter_map(|(k, v)| v.map(|v| (z0 + h * (k as f64 + 0.5) / bands as f64, v))).collect()
}

/// A waist: (base radius, waist radius, waist height as a share of the height, top radius).
/// The waist is the narrowest band between the first and the last.
pub fn waist(pts: &[P3], bands: usize) -> Option<(f64, f64, f64, f64)> {
    let r = radii(pts, bands);
    if r.len() < 4 {
        return None;
    }
    let (z0, z1) = (pts.iter().map(|p| p[2]).fold(f64::MAX, f64::min), pts.iter().map(|p| p[2]).fold(f64::MIN, f64::max));
    let w = r[1..r.len() - 1].iter().min_by(|a, b| a.1.total_cmp(&b.1))?;
    Some((r[0].1, w.1, (w.0 - z0) / (z1 - z0), r[r.len() - 1].1))
}

/// Least-squares sphere with its centre over the plan centre of the bounds: (radius, share of
/// points within `tol` x radius of it, height of the points / diameter).
pub fn sphere(pts: &[P3], tol: f64) -> Option<(f64, f64, f64)> {
    if pts.len() < 20 {
        return None;
    }
    let lo = |i: usize| pts.iter().map(|p| p[i]).fold(f64::MAX, f64::min);
    let hi = |i: usize| pts.iter().map(|p| p[i]).fold(f64::MIN, f64::max);
    let (cx, cy) = ((lo(0) + hi(0)) / 2.0, (lo(1) + hi(1)) / 2.0);
    // |p - c|^2 = R^2 with c = (cx, cy, zc): x'^2 + y'^2 + z^2 = 2 zc z + (R^2 - zc^2), linear in z
    let n = pts.len() as f64;
    let (mut sz, mut szz, mut sd, mut szd) = (0.0, 0.0, 0.0, 0.0);
    for p in pts {
        let d = (p[0] - cx).powi(2) + (p[1] - cy).powi(2) + p[2] * p[2];
        sz += p[2];
        szz += p[2] * p[2];
        sd += d;
        szd += p[2] * d;
    }
    let den = n * szz - sz * sz;
    if den.abs() < 1e-9 * n * n {
        return None;
    }
    let slope = (n * szd - sz * sd) / den;
    let zc = slope / 2.0;
    let r2 = (sd - slope * sz) / n + zc * zc;
    if r2 <= 0.0 {
        return None;
    }
    let r = r2.sqrt();
    let on = pts.iter().filter(|p| ((p[0] - cx).hypot(p[1] - cy).hypot(p[2] - zc) - r).abs() <= tol * r).count();
    Some((r, on as f64 / n, (hi(2) - lo(2)) / (2.0 * r)))
}

impl Oracle for Form {
    fn name(&self) -> String {
        format!("geom {}", self.0.kind)
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let (name, p, f) = (self.name(), &self.0.p, cx.facts);
        match self.0.kind.as_str() {
            "twist_per_floor" => {
                let Some(deg) = num(p, "deg") else {
                    return no_capability(&name, "a twist per storey".into(), "missing parameter deg");
                };
                let tol = num(p, "tol").unwrap_or((deg.abs() * 0.25).max(0.1));
                let (steps_min, share_min) = (num(p, "steps_min").unwrap_or(3.0), num(p, "share_min").unwrap_or(0.8));
                let outlines = match storey_outlines(&cx.ed.model) {
                    Ok(o) => o,
                    Err(e) => return no_capability(&name, "slab outlines".into(), &format!("no capability: {e}")),
                };
                let turns = storey_turns(&outlines);
                let want = format!("each storey turned {deg} deg (+-{tol}) from the one below on >= {share_min} of >= {steps_min} steps");
                if turns.is_empty() {
                    return result(&name, want, format!("{} storey outline(s) with a heading, no step to measure", outlines.len()), false);
                }
                let good = turns.iter().filter(|t| (**t - deg).abs() <= tol).count();
                let mut s = turns.clone();
                s.sort_by(f64::total_cmp);
                let share = good as f64 / turns.len() as f64;
                result(
                    &name,
                    want,
                    format!("{good} of {} steps on the angle ({share:.2}), median {:.2} deg, total {:.1} deg", turns.len(), s[s.len() / 2], turns.iter().sum::<f64>()),
                    turns.len() as f64 >= steps_min && share + 1e-9 >= share_min,
                )
            }
            "waist" => {
                let (ratio_max, flare_min) = (num(p, "ratio_max").unwrap_or(0.85), num(p, "flare_min").unwrap_or(1.02));
                let (at_min, at_max) = (num(p, "at_min").unwrap_or(0.3), num(p, "at_max").unwrap_or(0.95));
                let want = format!("waist <= {ratio_max} of the base radius at {at_min}..{at_max} of the height, top >= {flare_min} x the waist");
                match waist(&f.points, num(p, "bands").unwrap_or(12.0) as usize) {
                    Some((base, w, at, top)) => {
                        let (ratio, flare) = (w / base.max(1e-9), top / w.max(1e-9));
                        result(
                            &name,
                            want,
                            format!("base radius {base:.1} m, waist {w:.1} m ({ratio:.2}) at {at:.2} of the height, top {top:.1} m ({flare:.2} x)"),
                            ratio <= ratio_max + 1e-9 && in_range(at, Some(at_min), Some(at_max)) && flare + 1e-9 >= flare_min,
                        )
                    }
                    None => result(&name, want, "too few heights with points to profile".into(), false),
                }
            }
            "sphere_fit" => {
                let (tol, share_min, extent_min) = (num(p, "tol").unwrap_or(0.05), num(p, "share_min").unwrap_or(0.8), num(p, "extent_min").unwrap_or(0.4));
                let radius = num(p, "radius");
                let rtol = num(p, "radius_tol").unwrap_or(0.1);
                let want = format!(
                    ">= {share_min} of nodes within {tol} R of one sphere{}, height >= {extent_min} of its diameter",
                    radius.map_or(String::new(), |r| format!(" of radius {r} m (+-{:.0} %)", rtol * 100.0))
                );
                match sphere(&f.points, tol) {
                    Some((r, share, extent)) => result(
                        &name,
                        want,
                        format!("radius {r:.2} m, {share:.2} of {} nodes on it, height {extent:.2} of the diameter", f.points.len()),
                        share + 1e-9 >= share_min && extent + 1e-9 >= extent_min && radius.is_none_or(|w| (r - w).abs() <= rtol * w),
                    ),
                    None => result(&name, want, format!("{} nodes: too few or flat, no sphere to fit", f.points.len()), false),
                }
            }
            "setback_per_floor" => {
                let Some(inset) = num(p, "inset") else {
                    return no_capability(&name, "a set-back per storey".into(), "missing parameter inset");
                };
                let tol = num(p, "tol").unwrap_or((inset.abs() * 0.25).max(0.1));
                let (steps_min, share_min) = (num(p, "steps_min").unwrap_or(3.0), num(p, "share_min").unwrap_or(0.8));
                let outlines = match storey_outlines(&cx.ed.model) {
                    Ok(o) => o,
                    Err(e) => return no_capability(&name, "slab outlines".into(), &format!("no capability: {e}")),
                };
                let d = storey_setbacks(&outlines);
                let want = format!("each storey set back {inset} m (+-{tol}) inside the one below on >= {share_min} of >= {steps_min} steps");
                if d.is_empty() {
                    return result(&name, want, format!("{} storey outline(s), no step to measure", outlines.len()), false);
                }
                let good = d.iter().filter(|x| (**x - inset).abs() <= tol).count();
                let share = good as f64 / d.len() as f64;
                let (lo, hi) = (d.iter().copied().fold(f64::MAX, f64::min), d.iter().copied().fold(f64::MIN, f64::max));
                result(
                    &name,
                    want,
                    format!("{good} of {} steps on the set-back ({share:.2}), {lo:.2}..{hi:.2} m", d.len()),
                    d.len() as f64 >= steps_min && share + 1e-9 >= share_min,
                )
            }
            "top_ratio" => {
                let (min, max) = (num(p, "min"), num(p, "max"));
                let want = format!("top radius / base radius in {}..{}", min.map_or("-".into(), |v| v.to_string()), max.map_or("-".into(), |v| v.to_string()));
                // elements: members, walls and solids (a masonry or cast tower), any other point on its own
                let mut elems: Vec<Vec<P3>> = f.members.iter().map(|m| vec![m.a, m.b]).collect();
                let ends: Vec<P3> = elems.iter().flatten().copied().collect();
                elems.extend(f.points.iter().filter(|p| !ends.contains(*p)).map(|p| vec![*p]));
                elems.extend(f.masses.iter().cloned());
                let bands = num(p, "bands").unwrap_or(10.0) as usize;
                // radii about the tower's own axis, not the centre of everything around it
                let r = tower_points(&elems).map_or(vec![], |(pts, axis)| radii_about(&pts, bands, axis));
                match (r.first(), r.last()) {
                    (Some(b), Some(t)) if r.len() >= 3 && b.1 > 0.0 => {
                        let ratio = t.1 / b.1;
                        result(&name, want, format!("base radius {:.1} m, top {:.1} m ({ratio:.2})", b.1, t.1), in_range(ratio, min, max))
                    }
                    _ => result(&name, want, "too few heights with points to profile".into(), false),
                }
            }
            _ => unreachable!("build admits only known kinds"),
        }
    }
}
