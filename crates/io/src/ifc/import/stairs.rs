//! Stairs, ramps and railings. Our own export carries the exact row in `DDC_Stair` /
//! `DDC_Ramp` / `DDC_Railing` and comes back unchanged. A stair from another file is fitted as a
//! straight flight from its body (run, width, rise, riser count) and listed as approximated; a
//! ramp or railing without our data, and a stair that does not fit, stay editable meshes with the
//! reason in the report. Nothing is dropped without an entry.

use std::collections::{HashMap, HashSet};

use ddc_geom::ramp::RampDef;
use ddc_geom::stair::{Railing, Shape, StairDef};
use ddc_geom::{v2, v3};
use ddc_model::{RailingHost, RailingSpec, RampSpec, StairSpec};

use crate::ifc::reader::StepFile;
use super::{ddc_nums, level_for, product_mesh, pset, tess, Plan, Rd, Why, V3};

#[derive(Debug, Clone, PartialEq)]
pub struct PStair {
    pub level: usize,
    /// `level` and `top_level` are filled at commit.
    pub spec: StairSpec,
    /// Name of the top level (None: the stair's own level).
    pub top: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PRailing {
    pub level: usize,
    pub spec: RailingSpec,
    /// Host stair (plan index) and side; None: the railing follows its own path.
    pub host: Option<(usize, u8)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PRamp {
    pub level: usize,
    /// `level` and `top_level` are filled at commit.
    pub spec: RampSpec,
    pub top: Option<String>,
}

/// What the plan has read about the file's relations.
pub(super) struct Cx<'a> {
    pub f: &'a StepFile,
    pub props: &'a HashMap<u32, Vec<u32>>,
    pub parts_of: &'a HashMap<u32, Vec<u32>>,
    pub aggregated: &'a HashMap<u32, u32>,
}

type Kv = HashMap<String, String>;

fn nums(kv: &Kv, key: &str, n: usize, bad: Why) -> Result<Vec<Option<f64>>, Why> {
    let v = ddc_nums(kv.get(key).ok_or(bad)?, bad)?;
    if v.len() == n {
        Ok(v)
    } else {
        Err(bad)
    }
}

fn level_z(pl: &Plan, level: usize, top: Option<&str>) -> (f64, f64) {
    let base = pl.levels[level].elevation;
    let up = top
        .and_then(|n| pl.levels.iter().find(|l| l.name == n))
        .map_or(base, |l| l.elevation);
    (base, up)
}

fn def_of(pl: &Plan, level: usize, s: &StairSpec, top: Option<&str>) -> StairDef {
    let (base, up) = level_z(pl, level, top);
    let z0 = base + s.base_offset;
    StairDef {
        origin: v2(s.origin.0, s.origin.1),
        angle: s.angle,
        base_z: z0,
        height: up + s.top_offset - z0,
        width: s.width,
        max_riser: s.max_riser,
        tread: s.tread,
        landing: s.landing,
        thickness: s.thickness,
        stringer: s.stringer,
        shape: s.shape,
    }
}

fn exact_stair(pl: &mut Plan, storey: Option<usize>, kv: &Kv) -> Result<PStair, Why> {
    let bad = "bad DDC_Stair";
    let v = nums(kv, "Spec", 12, bad)?;
    let req = |i: usize| v[i].ok_or(bad);
    let left = kv.get("Left").map(String::as_str) == Some("1");
    let first = match kv.get("First").map(String::as_str) {
        None | Some("-") => None,
        Some(t) => Some(t.parse::<usize>().map_err(|_| bad)?),
    };
    let gap = kv.get("Gap").and_then(|g| g.parse().ok()).unwrap_or(0.0);
    let shape = match kv.get("Shape").map(String::as_str) {
        Some("straight") => Shape::Straight,
        Some("l") => Shape::L { left, first },
        Some("u") => Shape::U { left, first, gap },
        Some("spiral") => {
            let num = |k: &str| kv.get(k).and_then(|v| v.parse::<f64>().ok()).ok_or(bad);
            Shape::Spiral {
                radius: num("Radius")?,
                core: num("Core")?,
                turn: num("Turn")?,
            }
        }
        _ => return Err(bad),
    };
    let spec = StairSpec {
        level: 0,
        top_level: 0,
        base_offset: req(0)?,
        top_offset: req(1)?,
        shape,
        origin: (req(2)?, req(3)?),
        angle: req(4)?,
        width: req(5)?,
        max_riser: req(6)?,
        tread: v[7],
        landing: req(8)?,
        thickness: req(9)?,
        stringer: v[10].zip(v[11]),
        material: kv.get("Material").cloned().unwrap_or_default(),
    };
    let level = level_for(pl, storey, 0.0);
    let top = kv.get("TopLevel").filter(|t| !t.is_empty()).cloned();
    def_of(pl, level, &spec, top.as_deref())
        .layout()
        .map_err(|_| bad)?;
    Ok(PStair { level, spec, top })
}

/// A straight flight found in a body.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Fit {
    /// Middle of the lower riser line.
    pub origin: (f64, f64),
    pub angle: f64,
    pub width: f64,
    pub run: f64,
    pub z0: f64,
    pub z1: f64,
}

/// Extents along the axis `a` (s) and the perpendicular on its right (t): s0, s1, t0, t1.
fn extents(pts: &[V3], a: f64) -> (f64, f64, f64, f64) {
    let (c, s) = (a.cos(), a.sin());
    let mut e = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for p in pts {
        let u = p[0] * c + p[1] * s;
        let t = p[0] * s - p[1] * c;
        e = (e.0.min(u), e.1.max(u), e.2.min(t), e.3.max(t));
    }
    e
}

/// Smallest plan rectangle around the points; the flight climbs along its long side.
pub(super) fn fit_straight(pts: &[V3]) -> Option<Fit> {
    if pts.len() < 4 || pts.iter().any(|p| p.iter().any(|c| !c.is_finite())) {
        return None;
    }
    let area = |a: f64| {
        let e = extents(pts, a);
        (e.1 - e.0) * (e.3 - e.2)
    };
    let mut best = (f64::MAX, 0.0);
    for k in 0..180 {
        let a = (k as f64).to_radians();
        let v = area(a);
        if v < best.0 {
            best = (v, a);
        }
    }
    let center = best.1;
    for k in -20..=20 {
        let a = center + k as f64 * 0.05f64.to_radians();
        let v = area(a);
        if v < best.0 {
            best = (v, a);
        }
    }
    let e = extents(pts, best.1);
    let mut angle = if e.1 - e.0 >= e.3 - e.2 {
        best.1
    } else {
        best.1 + std::f64::consts::FRAC_PI_2
    };
    // the upper end of the flight is the end where the body is higher
    let e = extents(pts, angle);
    let (c, s) = (angle.cos(), angle.sin());
    let mid = (e.0 + e.1) / 2.0;
    let (mut lo, mut hi) = ((0.0, 0usize), (0.0, 0usize));
    for p in pts {
        let side = if p[0] * c + p[1] * s < mid {
            &mut lo
        } else {
            &mut hi
        };
        side.0 += p[2];
        side.1 += 1;
    }
    if lo.1 == 0 || hi.1 == 0 {
        return None;
    }
    if lo.0 / lo.1 as f64 > hi.0 / hi.1 as f64 {
        angle += std::f64::consts::PI;
    }
    let e = extents(pts, angle);
    let (c, s) = (angle.cos(), angle.sin());
    let tm = (e.2 + e.3) / 2.0;
    let (z0, z1) = pts.iter().fold((f64::MAX, f64::MIN), |z, p| {
        (z.0.min(p[2]), z.1.max(p[2]))
    });
    Some(Fit {
        origin: (c * e.0 + s * tm, s * e.0 - c * tm),
        angle,
        width: e.3 - e.2,
        run: e.1 - e.0,
        z0,
        z1,
    })
}

fn fit_stair(
    pl: &mut Plan,
    storey: Option<usize>,
    m: &tess::Tri,
    risers: Option<f64>,
    riser_height: Option<f64>,
) -> Result<PStair, Why> {
    let pts: Vec<V3> = m.t.iter().flatten().map(|&i| m.p[i as usize]).collect();
    let fit = fit_straight(&pts).ok_or("stair shape not recognised")?;
    let h = fit.z1 - fit.z0;
    if !(h > 0.1 && fit.width > 0.3 && fit.run > 0.3) {
        return Err("stair too small to fit");
    }
    let hinted = risers
        .map(f64::round)
        .filter(|n| *n >= 2.0 && h / n >= 0.08 && h / n <= 0.25);
    // a body that stops one riser below the upper floor (what our export writes) is told by the
    // flight's own riser height; its total rise is then one riser more than the body
    let (n, h) = match (hinted, riser_height) {
        (Some(n), Some(rh)) if rh > 0.0 && (h / rh - (n - 1.0)).abs() < 0.2 => (n, h + rh),
        (Some(n), _) => (n, h),
        _ => ((h / 0.17).round().max(2.0), h),
    };
    let n = n as usize;
    let riser = h / n as f64;
    let level = level_for(pl, storey, fit.z0);
    let base = pl.levels[level].elevation;
    let mut spec = StairSpec::straight(0, 0, fit.origin, fit.angle);
    spec.base_offset = fit.z0 - base;
    spec.top_offset = fit.z0 + h - base;
    spec.width = fit.width;
    // keeps the riser count: ceil(h / max_riser) = n
    spec.max_riser = riser + 1e-6;
    spec.tread = Some(fit.run / (n - 1) as f64);
    def_of(pl, level, &spec, None)
        .layout()
        .map_err(|_| "fitted stair breaks the comfort rule")?;
    Ok(PStair {
        level,
        spec,
        top: None,
    })
}

fn seg_dist(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let d = (b.0 - a.0, b.1 - a.1);
    let l2 = d.0 * d.0 + d.1 * d.1;
    let t = if l2 < 1e-24 {
        0.0
    } else {
        (((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1) / l2).clamp(0.0, 1.0)
    };
    (a.0 + d.0 * t - p.0).hypot(a.1 + d.1 * t - p.1)
}

/// Railing on the edge of fitted stair `ix` nearer to its body; height = top of the body above
/// the top nosing of that edge. Spacings and sections keep the defaults.
fn fit_railing(pl: &Plan, ix: usize, m: &tess::Tri) -> Result<PRailing, Why> {
    let st = &pl.stairs[ix];
    let lay = def_of(pl, st.level, &st.spec, st.top.as_deref())
        .layout()
        .map_err(|_| "host stair has no layout")?;
    let paths = lay.edge_paths();
    let pts: Vec<V3> = m.t.iter().flatten().map(|&i| m.p[i as usize]).collect();
    if pts.is_empty() {
        return Err("no body");
    }
    let n = pts.len() as f64;
    let c = (
        pts.iter().map(|p| p[0]).sum::<f64>() / n,
        pts.iter().map(|p| p[1]).sum::<f64>() / n,
    );
    let dist = |path: &[ddc_geom::V3]| {
        path.windows(2)
            .map(|w| seg_dist(c, (w[0].x, w[0].y), (w[1].x, w[1].y)))
            .fold(f64::INFINITY, f64::min)
    };
    let d = [dist(&paths[0]), dist(&paths[1])];
    let side: u8 = if d[0] <= d[1] { 0 } else { 1 };
    if d[side as usize] > 0.3 {
        return Err("railing does not follow an edge of its stair");
    }
    let top = pts.iter().map(|p| p[2]).fold(f64::MIN, f64::max);
    let nosing = paths[side as usize]
        .iter()
        .map(|p| p.z)
        .fold(f64::MIN, f64::max);
    let height = ((top - nosing) * 1e9).round() / 1e9;
    if !(0.3..=2.0).contains(&height) {
        return Err("railing height out of range");
    }
    let mut spec = RailingSpec::new(0, RailingHost::Stair { stair: 0, side });
    spec.height = height;
    Ok(PRailing {
        level: st.level,
        spec,
        host: Some((ix, side)),
    })
}

fn parse_points(t: &str, dim: usize) -> Option<Vec<Vec<f64>>> {
    let out: Option<Vec<Vec<f64>>> = t
        .split(';')
        .map(|p| {
            let v: Option<Vec<f64>> = p
                .split(',')
                .map(|x| x.parse::<f64>().ok().filter(|v| v.is_finite()))
                .collect();
            v.filter(|v| v.len() == dim)
        })
        .collect();
    out.filter(|v| v.len() >= 2)
}

fn exact_railing(
    pl: &mut Plan,
    storey: Option<usize>,
    kv: &Kv,
    stair_of: &HashMap<String, usize>,
) -> Result<PRailing, Why> {
    let bad = "bad DDC_Railing";
    let v = nums(kv, "Spec", 7, bad)?;
    let req = |i: usize| v[i].ok_or(bad);
    let (host, spec_host) = match kv.get("Host").map(String::as_str) {
        Some("stair") => {
            let side = kv
                .get("Side")
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(0)
                .min(1);
            let ix = *kv
                .get("HostStair")
                .and_then(|g| stair_of.get(g))
                .ok_or("host stair not imported")?;
            (Some((ix, side)), RailingHost::Stair { stair: 0, side })
        }
        Some("path") => {
            let pts = parse_points(kv.get("Path").ok_or(bad)?, 3).ok_or(bad)?;
            let pts = pts.iter().map(|p| (p[0], p[1], p[2])).collect();
            (None, RailingHost::Path(pts))
        }
        _ => return Err(bad),
    };
    let mut spec = RailingSpec::new(0, spec_host);
    spec.height = req(0)?;
    spec.post_spacing = req(1)?;
    spec.baluster_spacing = req(2)?;
    spec.post = req(3)?;
    spec.baluster = req(4)?;
    spec.rail = (req(5)?, req(6)?);
    spec.material = kv.get("Material").cloned().unwrap_or_default();
    let path = match &spec.host {
        RailingHost::Path(p) => p.iter().map(|q| v3(q.0, q.1, q.2)).collect(),
        RailingHost::Stair { .. } => vec![v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0)],
    };
    Railing {
        path,
        height: spec.height,
        post_spacing: spec.post_spacing,
        baluster_spacing: spec.baluster_spacing,
        post: spec.post,
        baluster: spec.baluster,
        rail: spec.rail,
    }
    .check()
    .map_err(|_| bad)?;
    let level = level_for(pl, storey, 0.0);
    Ok(PRailing { level, spec, host })
}

fn exact_ramp(pl: &mut Plan, storey: Option<usize>, kv: &Kv) -> Result<PRamp, Why> {
    let bad = "bad DDC_Ramp";
    let v = nums(kv, "Spec", 7, bad)?;
    let req = |i: usize| v[i].ok_or(bad);
    let path = parse_points(kv.get("Path").ok_or(bad)?, 2).ok_or(bad)?;
    let path: Vec<(f64, f64)> = path.iter().map(|p| (p[0], p[1])).collect();
    let mut spec = RampSpec::new(0, None, path);
    spec.base_offset = req(0)?;
    spec.top_offset = req(1)?;
    spec.width = req(2)?;
    spec.slope = req(3)?;
    spec.thickness = req(4)?;
    spec.landing = req(5)?;
    spec.max_run = req(6)?;
    spec.material = kv.get("Material").cloned().unwrap_or_default();
    let level = level_for(pl, storey, 0.0);
    let top = kv.get("TopLevel").filter(|t| !t.is_empty()).cloned();
    let (base, up) = level_z(pl, level, top.as_deref());
    let z0 = base + spec.base_offset;
    let z1 = match &top {
        Some(_) => up + spec.top_offset,
        None => z0 + spec.top_offset,
    };
    RampDef {
        path: spec.path.iter().map(|p| v2(p.0, p.1)).collect(),
        base_z: z0,
        rise: z1 - z0,
        width: spec.width,
        slope: spec.slope,
        thickness: spec.thickness,
        landing: spec.landing,
        max_run: spec.max_run,
    }
    .layout()
    .map_err(|_| bad)?;
    Ok(PRamp { level, spec, top })
}

type Link<'a> = &'a dyn Fn(&mut Plan, &'static str, usize, u32, String);

/// Reads every stair, ramp and railing into the plan. Products that cannot become native get a
/// reason in `why_native` (they are meshed and listed); parts of a native one are marked done.
#[allow(clippy::too_many_arguments)]
pub(super) fn read(
    rd: &mut Rd,
    pl: &mut Plan,
    cx: &Cx,
    storey_of: &dyn Fn(u32) -> Option<usize>,
    guid_of: &dyn Fn(u32) -> String,
    link: Link,
    done: &mut HashSet<u32>,
    why_native: &mut HashMap<u32, Why>,
) {
    let f = cx.f;
    let kids = |id: u32| cx.parts_of.get(&id).cloned().unwrap_or_default();
    let mut stair_of: HashMap<String, usize> = HashMap::new();
    // stairs fitted from a body: entity → plan index
    let mut fitted: HashMap<u32, usize> = HashMap::new();

    for (id, ty) in f.iter() {
        if ty != "IFCSTAIR" || done.contains(&id) {
            continue;
        }
        let Some(kv) = pset(f, cx.props, id, "DDC_Stair") else {
            continue;
        };
        match exact_stair(pl, storey_of(id), &kv) {
            Ok(p) => {
                let n = pl.stairs.len();
                pl.stairs.push(p);
                let g = guid_of(id);
                stair_of.insert(g.clone(), n);
                link(pl, "stair", n, id, g);
                done.insert(id);
                done.extend(kids(id));
            }
            Err(why) => {
                why_native.insert(id, why);
            }
        }
    }

    for (id, ty) in f.iter() {
        if !matches!(ty, "IFCSTAIR" | "IFCSTAIRFLIGHT") || done.contains(&id) {
            continue;
        }
        let Some(e) = f.get(id) else { continue };
        // a flight of a stair is read with that stair
        if ty == "IFCSTAIRFLIGHT"
            && cx
                .aggregated
                .get(&id)
                .is_some_and(|p| f.type_of(*p) == Some("IFCSTAIR"))
        {
            continue;
        }
        let parts = kids(id);
        let own = Rd::arg(e, 6).as_ref().is_some();
        // stringers and railings ride along: a flight with no landing is still one straight run
        let (flights, rest): (Vec<u32>, Vec<u32>) = parts
            .iter()
            .copied()
            .filter(|p| !matches!(f.type_of(*p), Some("IFCMEMBER" | "IFCRAILING")))
            .partition(|p| f.type_of(*p) == Some("IFCSTAIRFLIGHT"));
        let src = if own {
            id
        } else {
            if parts.is_empty() {
                continue;
            }
            if flights.len() != 1 || !rest.is_empty() {
                *pl.report
                    .as_mesh
                    .entry("IFCSTAIR: several flights or landings".into())
                    .or_default() += 1;
                continue;
            }
            flights[0]
        };
        let Some(se) = f.get(src) else { continue };
        let hint = Rd::arg(se, 8).as_f64();
        let riser_height = Rd::arg(se, 10).as_f64();
        let fit = product_mesh(rd, se)
            .and_then(|(m, _)| fit_stair(pl, storey_of(id), &m, hint, riser_height));
        match fit {
            Ok(p) => {
                let n = pl.stairs.len();
                pl.stairs.push(p);
                link(pl, "stair", n, id, guid_of(id));
                fitted.insert(id, n);
                done.insert(id);
                for &k in &parts {
                    match f.type_of(k) {
                        Some("IFCRAILING") => {}
                        // the stair's own body already shows its parts
                        _ if own => {
                            done.insert(k);
                        }
                        Some("IFCMEMBER") => {
                            why_native.insert(k, "stringer of a fitted stair");
                        }
                        _ => {
                            done.insert(k);
                        }
                    }
                }
                *pl.report
                    .approximated
                    .entry(format!("{ty}: straight stair fitted from its body"))
                    .or_default() += 1;
            }
            Err(why) => {
                why_native.insert(id, why);
            }
        }
    }

    for (id, ty) in f.iter() {
        if ty != "IFCRAILING" || done.contains(&id) {
            continue;
        }
        let Some(kv) = pset(f, cx.props, id, "DDC_Railing") else {
            let host = cx.aggregated.get(&id).and_then(|s| fitted.get(s)).copied();
            let Some(ix) = host else {
                why_native.insert(id, "no DDC_Railing data");
                continue;
            };
            let r = f
                .get(id)
                .ok_or("missing entity")
                .and_then(|e| product_mesh(rd, e))
                .and_then(|(m, _)| fit_railing(pl, ix, &m));
            match r {
                Ok(p) => {
                    let n = pl.railings.len();
                    pl.railings.push(p);
                    link(pl, "railing", n, id, guid_of(id));
                    done.insert(id);
                    *pl.report
                        .approximated
                        .entry(format!("{ty}: railing fitted to the edge of its stair"))
                        .or_default() += 1;
                }
                Err(why) => {
                    why_native.insert(id, why);
                }
            }
            continue;
        };
        match exact_railing(pl, storey_of(id), &kv, &stair_of) {
            Ok(p) => {
                let n = pl.railings.len();
                pl.railings.push(p);
                link(pl, "railing", n, id, guid_of(id));
                done.insert(id);
                done.extend(kids(id));
            }
            Err(why) => {
                why_native.insert(id, why);
            }
        }
    }

    for (id, ty) in f.iter() {
        if ty != "IFCRAMP" || done.contains(&id) {
            continue;
        }
        let Some(kv) = pset(f, cx.props, id, "DDC_Ramp").filter(|k| k.contains_key("Spec")) else {
            why_native.insert(id, "no DDC_Ramp data");
            // the flights carry the body, so they are the ones that become meshes
            for k in kids(id) {
                why_native.insert(k, "no DDC_Ramp data");
            }
            continue;
        };
        match exact_ramp(pl, storey_of(id), &kv) {
            Ok(p) => {
                let n = pl.ramps.len();
                pl.ramps.push(p);
                link(pl, "ramp", n, id, guid_of(id));
                done.insert(id);
                done.extend(kids(id));
            }
            Err(why) => {
                why_native.insert(id, why);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Points of a straight flight: `n` risers of 0.17 m, tread 0.28 m, width 1.2 m, at `a`.
    fn flight(n: usize, a: f64) -> Vec<V3> {
        let (c, s) = (a.cos(), a.sin());
        let mut v = vec![];
        for k in 0..n {
            for (u, z) in [(0.28 * k as f64, 0.17 * (k + 1) as f64), (0.28 * k as f64, 0.0)] {
                for t in [-0.6, 0.6] {
                    // right of the axis is (s, -c)
                    v.push([u * c + t * s + 2.0, u * s - t * c + 1.0, z]);
                }
            }
        }
        v
    }

    #[test]
    fn a_straight_flight_is_found_in_any_direction() {
        for deg in [0.0f64, 30.0, 90.0, 135.0, 200.0, 315.0] {
            let a = deg.to_radians();
            let fit = fit_straight(&flight(10, a)).unwrap();
            let d = (fit.angle - a).rem_euclid(std::f64::consts::TAU);
            assert!(d < 0.02 || d > std::f64::consts::TAU - 0.02, "{deg}: {d}");
            assert!((fit.width - 1.2).abs() < 0.01, "{deg}");
            assert!((fit.run - 0.28 * 9.0).abs() < 0.01, "{deg}");
            assert!((fit.z1 - fit.z0 - 1.7).abs() < 1e-9);
        }
    }

    #[test]
    fn nothing_is_fitted_to_a_flat_or_empty_body() {
        assert!(fit_straight(&[]).is_none());
        assert!(fit_straight(&[[0.0, 0.0, 0.0]; 3]).is_none());
    }

    #[test]
    fn a_fitted_stair_keeps_its_riser_count_and_rise() {
        let mut pl = Plan::default();
        pl.levels.push(super::super::PLevel {
            name: "L1".into(),
            elevation: 0.0,
        });
        let pts = flight(10, 0.3);
        let m = tess::Tri {
            t: (0..pts.len() as u32 - 2).map(|i| [i, i + 1, i + 2]).collect(),
            p: pts,
        };
        let p = fit_stair(&mut pl, Some(0), &m, Some(10.0), None).unwrap();
        let d = def_of(&pl, p.level, &p.spec, None).layout().unwrap();
        assert_eq!(d.risers, 10);
        assert!((d.z.1 - d.z.0 - 1.7).abs() < 1e-6);
    }

    #[test]
    fn a_stair_that_breaks_the_comfort_rule_is_refused_with_a_reason() {
        let mut pl = Plan::default();
        pl.levels.push(super::super::PLevel {
            name: "L1".into(),
            elevation: 0.0,
        });
        // 1.7 m of rise over a 1.2 m run: far from 2R + T = 0.63
        let pts: Vec<V3> = vec![
            [0.0, 0.0, 0.0],
            [0.5, 0.0, 1.7],
            [0.0, 1.2, 0.0],
            [0.5, 1.2, 1.7],
        ];
        let m = tess::Tri {
            t: vec![[0, 1, 2], [1, 2, 3]],
            p: pts,
        };
        assert_eq!(
            fit_stair(&mut pl, Some(0), &m, None, None).unwrap_err(),
            "fitted stair breaks the comfort rule"
        );
    }

    #[test]
    fn point_lists_need_the_right_width_and_two_points() {
        assert!(parse_points("1.0,2.0;3.0,4.0", 2).is_some());
        assert!(parse_points("1.0,2.0", 2).is_none());
        assert!(parse_points("1.0,2.0,3.0;4.0,5.0", 3).is_none());
        assert!(parse_points("1.0,x;3.0,4.0", 2).is_none());
    }
}
