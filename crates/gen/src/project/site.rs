//! The site: the zoning envelope a plot and its rules allow, the terrain under the building,
//! the ground-floor level a rule picks on it, the pad with its retaining walls and the
//! terraces of a stepped plinth, the approach from every exterior door to grade, and the cut
//! and fill. Every number is a rule; a broken limit is a conflict that names its rule. Site
//! coordinates (plot, terrain) are in the brief's frame, before `origin` moves the project.

use super::footprint::{self, Plate, Region, P};
use super::offset;
use super::rules::{self, rule, RuleSet};
use super::why::s as show;
use super::why_text::Args;
use super::{GProject, ProjectSpec};
use crate::{GenError, WallRole};
use ddc_geom::topo::{Tin, TinInput};

mod split;
pub use split::{barrier_free, Access, Face, GAccess, GCut, GSplit};
use ddc_geom::{v2, v3, V3};
use serde_json::Value as J;

/// The terrain a brief gives.
#[derive(Clone, Debug, PartialEq)]
pub enum Terrain {
    /// Level `z` at the plan origin, falling `slope` per metre along `downhill`.
    Plane { z: f64, slope: f64, downhill: P },
    /// Survey points `(x, y, z)`, triangulated; from the brief or the model's terrain.
    Points(Vec<(f64, f64, f64)>),
}

/// How the ground-floor level is chosen (`site.ground_rule`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ground {
    /// Cut and fill under the pad balance.
    Balance,
    /// The main entrance opens at grade.
    Entrance,
}

/// How split levels reach both half-levels where no lift car opens on both (`site.split_lift`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitLift {
    /// A platform lift beside each half-level flight, else a second lift.
    Platform,
    /// A second lift on the half-levels the first misses, else platform lifts.
    Lift,
}

/// What the brief says about the site, with the rule values its fields stand for.
#[derive(Clone, Debug, PartialEq)]
pub struct SiteSpec {
    /// The plot counter-clockwise: `site.plot`, else the straight `footprint.plot`.
    pub plot: Option<Vec<P>>,
    /// Set-back of each plot edge (edge i runs from point i to i + 1); empty: `uniform`.
    pub setbacks: Vec<f64>,
    /// `footprint.plot_setback`, the set-back of every edge without its own.
    pub uniform: f64,
    /// Plot edges whose facade must stand on the set-back line.
    pub build_to: Vec<usize>,
    pub max_storeys: u32,
    pub max_height: f64,
    pub coverage_max: f64,
    pub far_max: f64,
    pub terrain: Option<Terrain>,
    pub ground: Ground,
    pub pad_margin: f64,
    pub step_slope: f64,
    pub split_slope: f64,
    pub split_lift: SplitLift,
    /// Locks: the ground-floor level kept from an earlier run, and the plinth form (flat,
    /// stepped or split levels).
    pub lock_ground: Option<f64>,
    pub lock_stepped: Option<bool>,
    pub lock_split: Option<bool>,
}

/// A level platform: the building pad or a terrace of the plinth; `z` absolute.
#[derive(Clone, Debug, PartialEq)]
pub struct GPad {
    pub ring: Vec<P>,
    pub z: f64,
}

/// A retaining wall on plan from `a` to `b`, holding the ground between `base` and `top`
/// (absolute). `cut`: the terrain above the pad; else the pad above the terrain. Terrace 0
/// stands on the pad edge, terrace k is k terrace depths further out.
#[derive(Clone, Debug, PartialEq)]
pub struct GRetain {
    pub a: P,
    pub b: P,
    pub base: f64,
    pub top: f64,
    pub cut: bool,
    pub terrace: u32,
    pub thickness: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approach {
    Grade,
    Ramp,
    Steps,
}

/// How an exterior door reaches the terrain: `kind` is `None` when nothing fits the plot.
#[derive(Clone, Debug, PartialEq)]
pub struct GApproach {
    pub door: P,
    /// Where the approach leaves the pad, and its direction away from the building.
    pub foot: P,
    pub dir: P,
    pub kind: Option<Approach>,
    /// Terrain above the floor at the foot, m (negative: the terrain lies lower).
    pub rise: f64,
    /// Plan length of the ramp or steps.
    pub run: f64,
    /// The first exterior door of the ground floor.
    pub main: bool,
}

/// What the site part of the build produced; empty without a plot or terrain.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GSite {
    /// What the plot and its set-backs allow on plan; empty without a plot.
    pub envelope: Vec<P>,
    /// A terrain was given and everything below is measured on it.
    pub terrain: bool,
    /// Absolute level of the ground floor: the project's level 0.
    pub ground_z: f64,
    /// `balance`, `entrance` or `locked`.
    pub ground_by: String,
    /// Fall of the terrain per metre under the pad, and its downhill direction.
    pub slope: f64,
    pub downhill: P,
    pub stepped: bool,
    pub pads: Vec<GPad>,
    pub walls: Vec<GRetain>,
    pub approaches: Vec<GApproach>,
    /// Earthworks of the pad (terrain above it, below it) and of the basements, m3.
    pub cut: f64,
    pub fill: f64,
    pub excavation: f64,
    /// Ground cover share of the plot and floor-area ratio, 0 without a plot.
    pub coverage: f64,
    pub far: f64,
    pub warnings: Vec<String>,
    /// Split levels, when the slope passes `site.split_slope` and the building can part.
    pub split: Option<GSplit>,
    /// Why the building stays on one level on a terrain; empty when it is split.
    pub split_why: String,
}

impl GSite {
    pub(super) fn shift(&mut self, o: P) {
        let mv = |q: &mut P| *q = (q.0 + o.0, q.1 + o.1);
        if let Some(sp) = &mut self.split {
            sp.shift(o);
        }
        self.envelope.iter_mut().for_each(mv);
        for pad in &mut self.pads {
            pad.ring.iter_mut().for_each(mv);
        }
        for w in &mut self.walls {
            mv(&mut w.a);
            mv(&mut w.b);
        }
        for a in &mut self.approaches {
            mv(&mut a.door);
            mv(&mut a.foot);
        }
    }
}

/// A broken site limit: the rule, what the project has and what the rule allows.
#[derive(Clone, Debug, PartialEq)]
pub struct SiteConflict {
    pub rule: &'static str,
    pub value: f64,
    pub limit: f64,
    pub what: String,
}

impl SiteConflict {
    pub fn text(&self) -> String {
        format!("{}: {} (rule value {})", self.rule, self.what, self.limit)
    }
}

const KEYS: &[&str] = &[
    "plot",
    "setbacks",
    "build_to",
    "max_storeys",
    "max_height",
    "coverage_max",
    "far_max",
    "terrain",
    "ground",
    "pad_margin",
    "climate",
    "brief",
    "step_slope",
    "split_slope",
    "split_lift",
    "locks",
];

type Issues = Vec<(String, String)>;

fn def(id: &str) -> &'static rules::RuleDef {
    rules::def(id).unwrap_or_else(|| panic!("rule {id} is not in rules.json"))
}

/// A rule's value in the spec's layers, else its built-in default.
fn layered(rs: &RuleSet, id: &str) -> f64 {
    let d = def(id);
    rs.value(id).or(d.default).unwrap_or(d.min)
}

/// `site.<key>`, a brief field of rule `id`, in the rule's range.
fn num(issues: &mut Issues, s: &J, key: &str, id: &str, rs: &RuleSet) -> f64 {
    let d = def(id);
    let fallback = layered(rs, id);
    match &s[key] {
        J::Null => fallback,
        v => match v.as_f64() {
            Some(x) if d.contains(x) && (!d.int || x.fract() == 0.0) => x,
            _ => {
                issues.push((format!("site.{key}"), format!("must be {}", d.takes())));
                fallback
            }
        },
    }
}

/// A straight ring of `[x, y]` points; `Err(true)` when it has arcs.
fn straight(v: &J) -> Result<Vec<P>, bool> {
    let a = v.as_array().ok_or(false)?;
    let mut out = vec![];
    for q in a {
        let x: Vec<f64> = q.as_array().ok_or(false)?.iter().map(|c| c.as_f64().ok_or(false)).collect::<Result<_, _>>()?;
        match x.as_slice() {
            [x, y] => out.push((*x, *y)),
            [x, y, b] if *b == 0.0 => out.push((*x, *y)),
            [_, _, _] => return Err(true),
            _ => return Err(false),
        }
    }
    Ok(out)
}

fn area(r: &[P]) -> f64 {
    footprint::signed_area(r).abs()
}

/// Where plot edge `i` lands once the ring is turned counter-clockwise.
fn turned(i: usize, n: usize, flip: bool) -> usize {
    if flip {
        (2 * n - 2 - i) % n
    } else {
        i
    }
}

fn edge_list(issues: &mut Issues, v: &J, path: &str, n: usize) -> Vec<usize> {
    let Some(a) = v.as_array() else {
        issues.push((path.into(), "must be a list of plot edge numbers".into()));
        return vec![];
    };
    let mut out = vec![];
    for x in a {
        match x.as_u64() {
            Some(i) if (i as usize) < n => out.push(i as usize),
            _ => issues.push((path.into(), format!("edge numbers run 0..{}", n.saturating_sub(1)))),
        }
    }
    out
}

fn read_terrain(issues: &mut Issues, t: &J) -> Option<Terrain> {
    let o = match t {
        J::Null => return None,
        J::Object(o) => o,
        J::String(s) if s == "model" => {
            issues.push(("site.terrain".into(), "'model' reads the model's terrain: build the brief in a model with a toposolid, or give points".into()));
            return None;
        }
        _ => {
            issues.push(("site.terrain".into(), "must be {slope, downhill, z?} or {points: [[x, y, z]...]}".into()));
            return None;
        }
    };
    for k in o.keys() {
        if !["slope", "downhill", "z", "points"].contains(&k.as_str()) {
            issues.push((format!("site.terrain.{k}"), "unknown field (slope, downhill, z, points)".into()));
        }
    }
    if let Some(pts) = o.get("points") {
        let list: Option<Vec<(f64, f64, f64)>> = pts.as_array().and_then(|a| {
            a.iter()
                .map(|q| match q.as_array().map(|x| x.iter().map(J::as_f64).collect::<Option<Vec<f64>>>()) {
                    Some(Some(x)) if x.len() == 3 && x.iter().all(|c| c.is_finite()) => Some((x[0], x[1], x[2])),
                    _ => None,
                })
                .collect()
        });
        let Some(list) = list else {
            issues.push(("site.terrain.points".into(), "must be a list of [x, y, z] in metres".into()));
            return None;
        };
        let v: Vec<V3> = list.iter().map(|q| v3(q.0, q.1, q.2)).collect();
        if let Err(e) = Tin::build(&TinInput { points: &v, ..TinInput::default() }) {
            issues.push(("site.terrain.points".into(), e.to_string()));
            return None;
        }
        return Some(Terrain::Points(list));
    }
    let top = def("site.step_slope").max;
    let slope = match o.get("slope").and_then(J::as_f64) {
        Some(s) if (0.0..=top).contains(&s) => s,
        _ => {
            issues.push(("site.terrain.slope".into(), format!("must be the fall per metre, 0..{top}")));
            return None;
        }
    };
    let d = o.get("downhill").and_then(J::as_array).and_then(|a| match (a.first().and_then(J::as_f64), a.get(1).and_then(J::as_f64)) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() && x.hypot(y) > rule("site.tolerance") => Some((x, y)),
        _ => None,
    });
    let Some(downhill) = d.or((slope == 0.0).then_some((0.0, -1.0))) else {
        issues.push(("site.terrain.downhill".into(), "must be a direction [dx, dy]".into()));
        return None;
    };
    let z = match o.get("z") {
        None | Some(J::Null) => 0.0,
        Some(x) => match x.as_f64().filter(|z| z.is_finite()) {
            Some(z) => z,
            None => {
                issues.push(("site.terrain.z".into(), "must be the level at the plan origin, m".into()));
                0.0
            }
        },
    };
    Some(Terrain::Plane { z, slope, downhill })
}

/// Reads `site` and the plot of `footprint.plot`; the problems go to `issues` with their path.
pub(super) fn read(v: &J, rs: &RuleSet, issues: &mut Issues) -> SiteSpec {
    let s = &v["site"];
    if let Some(o) = s.as_object() {
        for k in o.keys() {
            if !KEYS.contains(&k.as_str()) {
                issues.push((format!("site.{k}"), format!("unknown field (known: {})", KEYS.join(", "))));
            }
        }
    } else if !s.is_null() {
        issues.push(("site".into(), "must be an object".into()));
    }
    let tol = rule("site.tolerance");
    let mut plot = None;
    let mut flip = false;
    let given = if s["plot"].is_null() { &v["footprint"]["plot"] } else { &s["plot"] };
    if !given.is_null() {
        match straight(given) {
            Ok(r) => {
                let n = r.len();
                let pts: Vec<_> = r.iter().map(|q| v2(q.0, q.1)).collect();
                let ok = n >= 3
                    && r.iter().all(|q| q.0.is_finite() && q.1.is_finite())
                    && ddc_geom::poly::simple_ring(&pts, tol).is_ok()
                    && area(&r) > tol;
                if ok {
                    flip = footprint::signed_area(&r) < 0.0;
                    let mut r = r;
                    if flip {
                        r.reverse();
                    }
                    plot = Some(r);
                } else if !s["plot"].is_null() {
                    issues.push(("site.plot".into(), "must be a simple ring of at least 3 points".into()));
                }
            }
            Err(true) if !s["plot"].is_null() => {
                issues.push(("site.plot".into(), "straight edges only: the set-backs are per edge".into()));
            }
            Err(_) => {
                if !s["plot"].is_null() {
                    issues.push(("site.plot".into(), "must be a list of [x, y] in metres".into()));
                }
            }
        }
    }
    let n = plot.as_ref().map_or(0, Vec::len);
    let uniform = v["footprint"]["plot_setback"].as_f64().unwrap_or_else(|| layered(rs, "site.plot_setback"));
    let mut setbacks = vec![];
    if let Some(a) = s["setbacks"].as_array() {
        let d = def("site.plot_setback");
        if a.len() != n {
            issues.push(("site.setbacks".into(), format!("one set-back per plot edge: {n}")));
        } else {
            let mut raw = vec![];
            for (i, x) in a.iter().enumerate() {
                match x.as_f64() {
                    Some(x) if d.contains(x) => raw.push(x),
                    _ => issues.push((format!("site.setbacks[{i}]"), format!("must be {}", d.takes()))),
                }
            }
            if raw.len() == n {
                setbacks = (0..n).map(|j| raw[turned(j, n, flip)]).collect();
            }
        }
    } else if !s["setbacks"].is_null() {
        issues.push(("site.setbacks".into(), "must be a list of metres, one per plot edge".into()));
    }
    let mut build_to = vec![];
    if !s["build_to"].is_null() {
        if n == 0 {
            issues.push(("site.build_to".into(), "needs site.plot or footprint.plot".into()));
        } else {
            build_to = edge_list(issues, &s["build_to"], "site.build_to", n).into_iter().map(|i| turned(i, n, flip)).collect();
            build_to.sort_unstable();
            build_to.dedup();
        }
    }
    let ground = match &s["ground"] {
        J::Null => layered(rs, "site.ground_rule"),
        g => def("site.ground_rule").parse_value(g).unwrap_or_else(|| {
            issues.push(("site.ground".into(), format!("must be {}", def("site.ground_rule").takes())));
            0.0
        }),
    };
    let ground = match def("site.ground_rule").options.get(ground as usize).map(String::as_str) {
        Some("entrance") => Ground::Entrance,
        _ => Ground::Balance,
    };
    let lift = match &s["split_lift"] {
        J::Null => layered(rs, "site.split_lift"),
        g => def("site.split_lift").parse_value(g).unwrap_or_else(|| {
            issues.push(("site.split_lift".into(), format!("must be {}", def("site.split_lift").takes())));
            0.0
        }),
    };
    let split_lift = match def("site.split_lift").options.get(lift as usize).map(String::as_str) {
        Some("lift") => SplitLift::Lift,
        _ => SplitLift::Platform,
    };
    let locks = &s["locks"];
    let mut lock_ground = None;
    let mut lock_stepped = None;
    let mut lock_split = None;
    if let Some(o) = locks.as_object() {
        for (k, x) in o {
            match (k.as_str(), x) {
                ("ground_z", J::Number(z)) if z.as_f64().is_some_and(f64::is_finite) => lock_ground = z.as_f64(),
                ("plinth", J::String(f)) if f == "flat" || f == "stepped" => {
                    lock_stepped = Some(f == "stepped");
                    lock_split = Some(false);
                }
                ("plinth", J::String(f)) if f == "split" => lock_split = Some(true),
                _ => issues.push((format!("site.locks.{k}"), "known locks: ground_z (m), plinth (flat | stepped | split)".into())),
            }
        }
    } else if !locks.is_null() {
        issues.push(("site.locks".into(), "must be {ground_z?, plinth?}".into()));
    }
    let max_storeys = num(issues, s, "max_storeys", "site.max_storeys", rs);
    SiteSpec {
        plot,
        setbacks,
        uniform,
        build_to,
        max_storeys: max_storeys as u32,
        max_height: num(issues, s, "max_height", "site.max_height", rs),
        coverage_max: num(issues, s, "coverage_max", "site.coverage_max", rs),
        far_max: num(issues, s, "far_max", "site.far_max", rs),
        terrain: read_terrain(issues, &s["terrain"]),
        ground,
        pad_margin: num(issues, s, "pad_margin", "site.pad_margin", rs),
        step_slope: num(issues, s, "step_slope", "site.step_slope", rs),
        split_slope: num(issues, s, "split_slope", "site.split_slope", rs),
        split_lift,
        lock_ground,
        lock_stepped,
        lock_split,
    }
}

/// The plot with each edge's line moved in by its set-back, neighbours meeting at mitred
/// corners; `None` when an edge turns over or the ring crosses itself.
pub fn inset_edges(plot: &[P], s: &[f64]) -> Option<Vec<P>> {
    let n = plot.len();
    let tol = rule("site.tolerance");
    let line = |i: usize| {
        let (a, b) = (plot[i], plot[(i + 1) % n]);
        let l = (b.0 - a.0).hypot(b.1 - a.1);
        let u = ((b.0 - a.0) / l, (b.1 - a.1) / l);
        ((a.0 - u.1 * s[i], a.1 + u.0 * s[i]), u)
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (p0, u0) = line((i + n - 1) % n);
        let (p1, u1) = line(i);
        let cr = u0.0 * u1.1 - u0.1 * u1.0;
        if cr.abs() <= tol {
            out.push(p1);
            continue;
        }
        let d = (p1.0 - p0.0, p1.1 - p0.1);
        let t = (d.0 * u1.1 - d.1 * u1.0) / cr;
        out.push((p0.0 + u0.0 * t, p0.1 + u0.1 * t));
    }
    for i in 0..n {
        let (a, b) = (out[i], out[(i + 1) % n]);
        let (pa, pb) = (plot[i], plot[(i + 1) % n]);
        if (b.0 - a.0) * (pb.0 - pa.0) + (b.1 - a.1) * (pb.1 - pa.1) <= tol {
            return None;
        }
    }
    let pts: Vec<_> = out.iter().map(|q| v2(q.0, q.1)).collect();
    ddc_geom::poly::simple_ring(&pts, tol).ok()?;
    (footprint::signed_area(&out) > tol).then_some(out)
}

fn edge_setbacks(s: &SiteSpec, n: usize) -> Vec<f64> {
    if s.setbacks.len() == n {
        s.setbacks.clone()
    } else {
        vec![s.uniform; n]
    }
}

/// What the plot and its set-backs allow on plan: edge by edge when the brief gives
/// set-backs per edge or build-to lines, else the plot set back evenly as the plate is.
pub fn envelope(s: &SiteSpec) -> Option<Vec<P>> {
    let plot = s.plot.as_ref()?;
    if !s.setbacks.is_empty() || !s.build_to.is_empty() {
        return inset_edges(plot, &edge_setbacks(s, plot.len()));
    }
    if s.uniform <= 0.0 {
        return Some(plot.clone());
    }
    let r = Region { outer: plot.clone(), holes: vec![] };
    footprint::largest(offset::inset(&r, s.uniform)).map(|r| r.outer)
}

/// The largest footprint the coverage and floor-area limits allow on the plot, m2.
fn allowed(s: &SiteSpec, floors: u32) -> Option<(f64, &'static str)> {
    let a = area(s.plot.as_ref()?);
    let cov = s.coverage_max * a;
    let far = s.far_max * a / f64::from(floors.max(1));
    Some(if cov <= far { (cov, "site.coverage_max") } else { (far, "site.far_max") })
}

/// Shapes a plot plate inside its envelope: each plot edge in by its set-back, then every
/// edge that is not a build-to line further in until coverage and floor-area ratio hold.
/// Nothing changes when the brief sets neither per-edge set-backs, build-to lines nor a
/// binding limit. The notes say what moved and by which rule.
pub(super) fn shape_plate(s: &SiteSpec, plate: &mut Plate, floors: u32, turned: bool) -> Result<Vec<String>, String> {
    let (Some(plot), Some((target, why))) = (s.plot.as_ref(), allowed(s, floors)) else {
        return Ok(vec![]);
    };
    let tol = rule("site.tolerance");
    let binding = footprint::region_area(&plate.base) > target * (1.0 + tol);
    if s.setbacks.is_empty() && s.build_to.is_empty() && !binding {
        return Ok(vec![]);
    }
    if turned {
        return Err("site: set-backs, build-to lines and coverage need footprint.rotation 0".into());
    }
    let n = plot.len();
    let base = edge_setbacks(s, n);
    let moved = |x: f64| {
        let sb: Vec<f64> = (0..n).map(|i| base[i] + if s.build_to.contains(&i) { 0.0 } else { x }).collect();
        inset_edges(plot, &sb)
    };
    let mut ring = moved(0.0).ok_or("site.plot_setback: the set-backs leave nothing of the plot")?;
    let mut notes = vec![];
    if area(&ring) > target * (1.0 + tol) {
        if s.build_to.len() >= n {
            return Err(format!("{why}: every plot edge is a build-to line, so the footprint cannot shrink to {target:.0} m2"));
        }
        let ((x0, y0), (x1, y1)) = footprint::bbox(plot);
        let (mut lo, mut hi) = (0.0, (x1 - x0).max(y1 - y0));
        while hi - lo > tol {
            let mid = (lo + hi) * 0.5;
            match moved(mid) {
                Some(r) if area(&r) > target => lo = mid,
                _ => hi = mid,
            }
        }
        ring = moved(hi)
            .filter(|r| area(r) <= target * (1.0 + tol))
            .ok_or_else(|| format!("{why}: no footprint of {target:.0} m2 or less fits the plot's set-backs"))?;
        notes.push(format!(
            "{why}: the facades off the build-to lines moved in {hi:.2} m more, footprint {:.0} m2",
            area(&ring)
        ));
    }
    plate.base = Region { outer: ring, holes: plate.base.holes.clone() };
    plate.pivot = footprint::centroid(&plate.base.outer);
    Ok(notes)
}

fn dist_seg(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (a.0 + dx * t - p.0).hypot(a.1 + dy * t - p.1)
}

fn dist_ring(p: P, r: &[P]) -> f64 {
    let n = r.len();
    (0..n).map(|i| dist_seg(p, r[i], r[(i + 1) % n])).fold(f64::MAX, f64::min)
}

fn inside(p: P, r: &[P]) -> bool {
    crate::check::inside(r, p)
}

fn slab_area(l: &crate::GLayout) -> f64 {
    area(&l.slab) - l.slab_holes.iter().map(|h| area(h)).sum::<f64>()
}

/// Length of `ring`'s edges lying along the line through `a` and `b`, within `tol` of it.
fn along(ring: &[P], a: P, b: P, tol: f64) -> f64 {
    let l = (b.0 - a.0).hypot(b.1 - a.1);
    let u = ((b.0 - a.0) / l, (b.1 - a.1) / l);
    let off = |q: P| ((q.0 - a.0) * -u.1 + (q.1 - a.1) * u.0).abs();
    let n = ring.len();
    (0..n)
        .filter(|&i| off(ring[i]) <= tol && off(ring[(i + 1) % n]) <= tol)
        .map(|i| {
            let (p, q) = (ring[i], ring[(i + 1) % n]);
            ((q.0 - p.0) * u.0 + (q.1 - p.1) * u.1).abs()
        })
        .sum()
}

/// Every site limit the project breaks. `o` moves the brief-frame plot into the project's
/// frame; the envelope is the project's own.
pub fn conflicts(spec: &ProjectSpec, p: &GProject, envelope: &[P], o: P) -> Vec<SiteConflict> {
    let s = &spec.site;
    let tol = rule("site.tolerance");
    let mut out = vec![];
    let storeys = p.storeys.len() as f64;
    if storeys > f64::from(s.max_storeys) {
        out.push(SiteConflict {
            rule: "site.max_storeys",
            value: storeys,
            limit: f64::from(s.max_storeys),
            what: format!("{storeys} storeys above ground"),
        });
    }
    let height = p.storeys.iter().map(|st| st.elevation + st.height).fold(0.0, f64::max);
    if height > s.max_height + tol {
        out.push(SiteConflict {
            rule: "site.max_height",
            value: height,
            limit: s.max_height,
            what: format!("{height:.2} m from the ground floor to the roof"),
        });
    }
    let Some(plot) = &s.plot else { return out };
    let plot: Vec<P> = plot.iter().map(|q| (q.0 + o.0, q.1 + o.1)).collect();
    let a = area(&plot);
    let etol = rule("site.envelope_tol");
    if !envelope.is_empty() {
        let out_of = p
            .storeys
            .iter()
            .flat_map(|st| st.layout.slab.iter())
            .map(|q| if inside(*q, envelope) { 0.0 } else { dist_ring(*q, envelope) })
            .fold(0.0, f64::max);
        if out_of > etol {
            out.push(SiteConflict {
                rule: "site.plot_setback",
                value: out_of,
                limit: s.uniform,
                what: format!("a slab corner stands {out_of:.2} m outside the set-backs"),
            });
        }
    }
    if let Some(g) = p.storeys.first() {
        let cov = slab_area(&g.layout) / a;
        if cov > s.coverage_max * (1.0 + tol) {
            out.push(SiteConflict {
                rule: "site.coverage_max",
                value: cov,
                limit: s.coverage_max,
                what: format!("the ground floor covers {cov:.3} of the plot"),
            });
        }
        let far = p.storeys.iter().map(|st| slab_area(&st.layout)).sum::<f64>() / a;
        if far > s.far_max * (1.0 + tol) {
            out.push(SiteConflict {
                rule: "site.far_max",
                value: far,
                limit: s.far_max,
                what: format!("floor-area ratio {far:.2}"),
            });
        }
        let n = envelope.len();
        if n == plot.len() {
            let share = rule("site.build_to_share");
            for &i in &s.build_to {
                let (ea, eb) = (envelope[i], envelope[(i + 1) % n]);
                let need = (eb.0 - ea.0).hypot(eb.1 - ea.1) * share;
                let got = along(&g.layout.slab, ea, eb, rule("site.build_to_tol"));
                if got + etol < need {
                    out.push(SiteConflict {
                        rule: "site.build_to_share",
                        value: got,
                        limit: share,
                        what: format!("{got:.1} m of facade on the build-to line of plot edge {i}, {need:.1} m needed"),
                    });
                }
            }
        }
    }
    out
}

/// The terrain as a level at any plan point.
enum Surface {
    Plane { z: f64, slope: f64, d: P },
    Tin(Box<Tin>),
}

impl Surface {
    fn new(t: &Terrain) -> Result<Surface, GenError> {
        match t {
            Terrain::Plane { z, slope, downhill } => {
                let l = downhill.0.hypot(downhill.1);
                Ok(Surface::Plane { z: *z, slope: *slope, d: (downhill.0 / l, downhill.1 / l) })
            }
            Terrain::Points(pts) => {
                let v: Vec<V3> = pts.iter().map(|q| v3(q.0, q.1, q.2)).collect();
                Tin::build(&TinInput { points: &v, ..TinInput::default() })
                    .map(|t| Surface::Tin(Box::new(t)))
                    .map_err(|e| GenError::Infeasible(format!("site.terrain: {e}")))
            }
        }
    }

    /// Off the survey, the level of the nearest survey point.
    fn z(&self, q: P) -> f64 {
        match self {
            Surface::Plane { z, slope, d } => z - slope * (q.0 * d.0 + q.1 * d.1),
            Surface::Tin(t) => {
                let at = v2(q.0, q.1);
                t.z_at(at)
                    .or_else(|| t.nearest_vertex(at).map(|i| t.pts[i as usize].z))
                    .unwrap_or(0.0)
            }
        }
    }
}

/// Centres of the `h` grid cells whose centre lies in `ring`.
fn cells(ring: &[P], h: f64) -> Vec<P> {
    let ((x0, y0), (x1, y1)) = footprint::bbox(ring);
    let nx = ((x1 - x0) / h).ceil() as usize;
    let ny = ((y1 - y0) / h).ceil() as usize;
    let mut out = vec![];
    for i in 0..nx {
        for j in 0..ny {
            let q = (x0 + (i as f64 + 0.5) * h, y0 + (j as f64 + 0.5) * h);
            if inside(q, ring) {
                out.push(q);
            }
        }
    }
    out
}

/// Cut (terrain above `z`) and fill (below) over `ring`, m3, on the `site.survey_step` grid.
pub fn cut_fill(ring: &[P], z: f64, t: &Terrain) -> Result<(f64, f64), GenError> {
    let surf = Surface::new(t)?;
    let h = rule("site.survey_step");
    Ok(volumes(&cells(ring, h), h, z, &surf))
}

fn volumes(cs: &[P], h: f64, z: f64, t: &Surface) -> (f64, f64) {
    let (mut cut, mut fill) = (0.0, 0.0);
    for q in cs {
        let dz = t.z(*q) - z;
        if dz > 0.0 {
            cut += dz * h * h;
        } else {
            fill -= dz * h * h;
        }
    }
    (cut, fill)
}

/// Least-squares plane through the sampled levels: fall per metre and downhill direction.
fn fall(cs: &[P], t: &Surface) -> (f64, P) {
    let k = cs.len() as f64;
    if cs.len() < 3 {
        return (0.0, (0.0, -1.0));
    }
    let zs: Vec<f64> = cs.iter().map(|q| t.z(*q)).collect();
    let mx = cs.iter().map(|q| q.0).sum::<f64>() / k;
    let my = cs.iter().map(|q| q.1).sum::<f64>() / k;
    let mz = zs.iter().sum::<f64>() / k;
    let (mut sxx, mut sxy, mut syy, mut sxz, mut syz) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (q, z) in cs.iter().zip(&zs) {
        let (x, y, z) = (q.0 - mx, q.1 - my, z - mz);
        sxx += x * x;
        sxy += x * y;
        syy += y * y;
        sxz += x * z;
        syz += y * z;
    }
    let det = sxx * syy - sxy * sxy;
    if det <= f64::EPSILON {
        return (0.0, (0.0, -1.0));
    }
    let b = (sxz * syy - syz * sxy) / det;
    let c = (syz * sxx - sxz * sxy) / det;
    let s = b.hypot(c);
    if s <= f64::EPSILON {
        (0.0, (0.0, -1.0))
    } else {
        (s, (-b / s, -c / s))
    }
}

/// An exterior door of the ground floor: its centre and outward normal.
struct Door {
    at: P,
    out: P,
}

fn doors(p: &GProject) -> Vec<Door> {
    let Some(g) = p.storeys.first() else { return vec![] };
    let l = &g.layout;
    let th = WallRole::Exterior.thickness();
    let mut out = vec![];
    for o in l.openings.iter().filter(|o| o.door) {
        let w = &l.walls[o.wall];
        if w.role != WallRole::Exterior {
            continue;
        }
        let len = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1);
        if len <= 0.0 {
            continue;
        }
        let u = ((w.b.0 - w.a.0) / len, (w.b.1 - w.a.1) / len);
        let t = o.along + o.width * 0.5;
        let at = (w.a.0 + u.0 * t, w.a.1 + u.1 * t);
        let n = (u.1, -u.0);
        let n = if inside((at.0 + n.0 * th, at.1 + n.1 * th), &l.slab) { (-n.0, -n.1) } else { n };
        out.push(Door { at, out: n });
    }
    out
}

/// Distance from `q` along `d` to the ring's boundary; `f64::MAX` when the ray leaves none.
fn ray(q: P, d: P, r: &[P]) -> f64 {
    let n = r.len();
    let mut best = f64::MAX;
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        let e = (b.0 - a.0, b.1 - a.1);
        let den = d.0 * e.1 - d.1 * e.0;
        if den.abs() <= f64::EPSILON {
            continue;
        }
        let w = (a.0 - q.0, a.1 - q.1);
        let t = (w.0 * e.1 - w.1 * e.0) / den;
        let s = (w.0 * d.1 - w.1 * d.0) / den;
        if t > 0.0 && (0.0..=1.0).contains(&s) {
            best = best.min(t);
        }
    }
    best
}

/// Where segment `a`-`b` is crossed by segment `c`-`d`: the parameter along `a`-`b`.
fn crossing(a: P, b: P, c: P, d: P) -> Option<f64> {
    let (e, f) = ((b.0 - a.0, b.1 - a.1), (d.0 - c.0, d.1 - c.1));
    let den = e.0 * f.1 - e.1 * f.0;
    if den.abs() <= f64::EPSILON {
        return None;
    }
    let w = (c.0 - a.0, c.1 - a.1);
    let t = (w.0 * f.1 - w.1 * f.0) / den;
    let s = (w.0 * e.1 - w.1 * e.0) / den;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&s)).then_some(t)
}

/// The retaining walls and terraces along one pad edge, and the approaches' gaps cut later.
#[allow(clippy::too_many_arguments)]
fn retain_edge(
    a: P,
    b: P,
    zg: f64,
    t: &Surface,
    stepped: bool,
    plot: Option<&[P]>,
    walls: &mut Vec<GRetain>,
    pads: &mut Vec<GPad>,
) {
    let len = (b.0 - a.0).hypot(b.1 - a.1);
    if len <= rule("site.tolerance") {
        return;
    }
    let u = ((b.0 - a.0) / len, (b.1 - a.1) / len);
    let out = (u.1, -u.0);
    let (h, rmin, rise, td) = (rule("site.survey_step"), rule("site.retain_min"), rule("site.step_rise"), rule("site.terrace_depth"));
    let m = (len / h).ceil().max(1.0) as usize;
    let step = len / m as f64;
    let dz: Vec<f64> = (0..m)
        .map(|k| {
            let s = (k as f64 + 0.5) * step;
            t.z((a.0 + u.0 * s, a.1 + u.1 * s)) - zg
        })
        .collect();
    let class = |x: f64| if x >= rmin { 1 } else if x <= -rmin { -1 } else { 0 };
    let mut k = 0;
    while k < m {
        let c = class(dz[k]);
        if c == 0 {
            k += 1;
            continue;
        }
        let k0 = k;
        let mut hmax: f64 = 0.0;
        while k < m && class(dz[k]) == c {
            hmax = hmax.max(dz[k].abs());
            k += 1;
        }
        let (s0, s1) = (k0 as f64 * step, k as f64 * step);
        let (pa, pb) = ((a.0 + u.0 * s0, a.1 + u.1 * s0), (a.0 + u.0 * s1, a.1 + u.1 * s1));
        let want = if stepped { (hmax / rise).ceil().max(1.0) as usize } else { 1 };
        let room = plot.map_or(f64::MAX, |r| dist_ring(pa, r).min(dist_ring(pb, r)));
        let fits = if td > 0.0 { ((room / td).floor() as usize).saturating_add(1) } else { 1 };
        let n = want.min(fits).max(1);
        let dh = hmax / n as f64;
        let cut = c > 0;
        for j in 0..n {
            let off = j as f64 * td;
            let (wa, wb) = ((pa.0 + out.0 * off, pa.1 + out.1 * off), (pb.0 + out.0 * off, pb.1 + out.1 * off));
            let (base, top) = if cut {
                (zg + j as f64 * dh, zg + (j + 1) as f64 * dh)
            } else {
                (zg - (j + 1) as f64 * dh, zg - j as f64 * dh)
            };
            walls.push(GRetain { a: wa, b: wb, base, top, cut, terrace: j as u32, thickness: rule("site.retain_thickness") });
            if j > 0 {
                let back = off - td;
                let ring = vec![
                    (pa.0 + out.0 * back, pa.1 + out.1 * back),
                    (pb.0 + out.0 * back, pb.1 + out.1 * back),
                    wb,
                    wa,
                ];
                let ring = footprint::clean(&ring, true).unwrap_or(ring);
                pads.push(GPad { ring, z: if cut { base } else { top } });
            }
        }
    }
}

/// Splits every retaining wall the approach crosses, leaving its width open.
fn open_gap(walls: &mut Vec<GRetain>, foot: P, dir: P, reach: f64) {
    let half = rule("egress.entrance_width") * 0.5;
    let end = (foot.0 + dir.0 * reach, foot.1 + dir.1 * reach);
    let back = rule("site.terrace_depth") * 0.5;
    let start = (foot.0 - dir.0 * back, foot.1 - dir.1 * back);
    let mut out = Vec::with_capacity(walls.len());
    for w in walls.drain(..) {
        let len = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1);
        match crossing(w.a, w.b, start, end) {
            Some(t) if len > 0.0 => {
                let (s0, s1) = (t * len - half, t * len + half);
                let at = |s: f64| (w.a.0 + (w.b.0 - w.a.0) * s / len, w.a.1 + (w.b.1 - w.a.1) * s / len);
                if s0 > rule("site.tolerance") {
                    out.push(GRetain { b: at(s0), ..w.clone() });
                }
                if len - s1 > rule("site.tolerance") {
                    out.push(GRetain { a: at(s1), ..w.clone() });
                }
            }
            _ => out.push(w),
        }
    }
    *walls = out;
}

/// The ground on the terrain: level, pad, retaining walls, terraces, approaches, volumes.
/// With split levels the pad parts on the line too, the lower half `drop` below the upper,
/// and a retaining wall holds the step between them.
fn ground(spec: &ProjectSpec, p: &GProject, t: &Surface, split: Option<&GSplit>, g: &mut GSite) {
    let s = &spec.site;
    let Some(slab) = p.storeys.first().and_then(|st| footprint::clean(&st.layout.slab, true)) else {
        return;
    };
    let tol = rule("site.tolerance");
    let room = s.plot.as_ref().map_or(f64::MAX, |r| slab.iter().map(|q| dist_ring(*q, r)).fold(f64::MAX, f64::min));
    let margin = s.pad_margin.min(room);
    let pad = if margin > tol {
        offset::outset(&Region { outer: slab.clone(), holes: vec![] }, margin).outer
    } else {
        slab.clone()
    };
    let h = rule("site.survey_step");
    // split, the court stays out of the pad so each wing's cut parts it on its own
    let court: Vec<Vec<P>> = p.court.iter().cloned().collect();
    let region = if margin > tol {
        offset::outset(&Region { outer: slab.clone(), holes: court.clone() }, margin)
    } else {
        Region { outer: slab.clone(), holes: court }
    };
    // each half-level's pads and how far their floor stands below the ground floor's level
    let parts: Vec<(Face, f64)> = match split.and_then(|sp| sp.pieces(&region.outer, &region.holes).map(|x| (sp, x))) {
        Some((sp, (hi, lo))) => hi.into_iter().map(|f| (f, 0.0)).chain(lo.into_iter().map(|f| (f, sp.drop))).collect(),
        None => vec![((pad.clone(), vec![]), 0.0)],
    };
    let below = |q: P| split.filter(|sp| sp.is_low(q)).map_or(0.0, |sp| sp.drop);
    let parted: Vec<(Vec<P>, f64)> = parts
        .iter()
        .map(|(f, d)| (cells(&f.0, h).into_iter().filter(|q| !f.1.iter().any(|x| inside(*q, x))).collect(), *d))
        .collect();
    let cs: Vec<P> = parted.iter().flat_map(|c| c.0.iter().copied()).collect();
    let mut sum = 0.0;
    for (c, d) in &parted {
        sum += c.iter().map(|q| t.z(*q) + d).sum::<f64>();
    }
    let mean = if cs.is_empty() { t.z(footprint::centroid(&pad)) } else { sum / cs.len() as f64 };
    let ds = doors(p);
    let th = WallRole::Exterior.thickness();
    let (raw, by) = match (s.lock_ground, s.ground, ds.first()) {
        (Some(z), _, _) => (z, "locked"),
        (None, Ground::Entrance, Some(d)) => (t.z((d.at.0 + d.out.0 * th, d.at.1 + d.out.1 * th)) + below(d.at), "entrance"),
        (None, Ground::Entrance, None) => {
            g.warnings.push("site.ground_rule: no exterior door on the ground floor, the cut and fill balance instead".into());
            (mean, "balance")
        }
        (None, Ground::Balance, _) => (mean, "balance"),
    };
    let snap = rule("site.level_snap");
    let zg = if s.lock_ground.is_some() { raw } else { (raw / snap).round() * snap };
    let (slope, downhill) = fall(&cs, t);
    let stepped = s.lock_stepped.unwrap_or(slope > s.step_slope);
    let (mut cut, mut fill) = (0.0, 0.0);
    for (c, d) in &parted {
        let v = volumes(c, h, zg - d, t);
        cut += v.0;
        fill += v.1;
    }
    let excavation = match (p.basements.last(), p.basements.first()) {
        (Some(low), Some(top)) => cells(&top.layout.slab, h)
            .iter()
            .map(|q| {
                let floor = zg - below(*q);
                (t.z(*q).min(floor) - (floor + low.elevation)).max(0.0) * h * h
            })
            .sum(),
        _ => 0.0,
    };
    let mut walls = vec![];
    let mut pads = vec![];
    for ((ring, _), d) in parts.iter().filter(|x| x.0 .0.len() >= 3) {
        let z = zg - d;
        pads.push(GPad { ring: ring.clone(), z });
        let n = ring.len();
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            // the edge on a cut is the step between the half-levels, held below
            if split.is_some_and(|sp| sp.on_line(a, b)) {
                continue;
            }
            retain_edge(a, b, z, t, stepped, s.plot.as_deref(), &mut walls, &mut pads);
        }
    }
    if let Some(sp) = split {
        let thickness = rule("site.retain_thickness");
        for (a, b, _) in sp.chords(&region.outer, &region.holes) {
            walls.push(GRetain { a, b, base: zg - sp.drop, top: zg, cut: true, terrace: 0, thickness });
        }
    }
    let (step_tol, ramp, riser, tread) = (rule("site.entrance_step"), rule("site.ramp_slope"), rule("site.riser"), rule("site.tread"));
    let mut approaches = vec![];
    for (i, d) in ds.iter().enumerate() {
        let low = split.is_some_and(|sp| sp.is_low(d.at));
        // the face of the door's half-level it opens from
        let near = |f: &Face| if inside(d.at, &f.0) { 0.0 } else { dist_ring(d.at, &f.0) };
        let pad = parts
            .iter()
            .filter(|x| (x.1 > 0.0) == low)
            .min_by(|x, y| near(&x.0).total_cmp(&near(&y.0)))
            .map_or(&pad, |x| &x.0 .0);
        let reach = ray(d.at, d.out, pad);
        let gap = if reach < f64::MAX { reach } else { th };
        let foot = (d.at.0 + d.out.0 * gap, d.at.1 + d.out.1 * gap);
        let rise = t.z(foot) - (zg - below(d.at));
        let ramp_run = rise.abs() / ramp;
        let steps_run = (rise.abs() / riser).ceil() * tread;
        let out_room = s.plot.as_ref().map_or(f64::MAX, |r| ray(foot, d.out, r));
        // or along the pad edge in front of the door, either way
        let np = pad.len();
        let along_room = (0..np)
            .map(|k| (pad[k], pad[(k + 1) % np]))
            .min_by(|x, y| dist_seg(foot, x.0, x.1).total_cmp(&dist_seg(foot, y.0, y.1)))
            .map_or(0.0, |(a, b)| (foot.0 - a.0).hypot(foot.1 - a.1).max((foot.0 - b.0).hypot(foot.1 - b.1)));
        let room = out_room.max(along_room);
        let (kind, run) = if rise.abs() <= step_tol {
            (Some(Approach::Grade), 0.0)
        } else if ramp_run <= room {
            (Some(Approach::Ramp), ramp_run)
        } else if steps_run <= room {
            (Some(Approach::Steps), steps_run)
        } else {
            (None, steps_run)
        };
        if kind.is_some() && kind != Some(Approach::Grade) {
            let terraces = walls.iter().map(|w| w.terrace).max().map_or(0.0, |k| f64::from(k + 1) * rule("site.terrace_depth"));
            open_gap(&mut walls, foot, d.out, run.max(terraces));
        }
        approaches.push(GApproach { door: d.at, foot, dir: d.out, kind, rise, run, main: i == 0 });
    }
    let rmax = rule("site.retain_max");
    for w in walls.iter().filter(|w| w.top - w.base > rmax) {
        g.warnings.push(format!(
            "site.retain_max: a retaining wall {:.2} m high at ({:.1}, {:.1})-({:.1}, {:.1}); a stepped plinth or more plot room would lower it",
            w.top - w.base, w.a.0, w.a.1, w.b.0, w.b.1
        ));
    }
    g.terrain = true;
    g.ground_z = zg;
    g.ground_by = by.into();
    g.slope = slope;
    g.downhill = downhill;
    g.stepped = stepped;
    g.pads = pads;
    g.walls = walls;
    g.approaches = approaches;
    g.cut = cut;
    g.fill = fill;
    g.excavation = excavation;
    g.split = split.cloned();
}

/// The site part of the build, on the project still at the brief's origin: refuses a broken
/// limit by its rule, then places the building on the terrain.
pub(super) fn apply(spec: &ProjectSpec, p: &mut GProject) -> Result<(), GenError> {
    let s = &spec.site;
    let mut g = GSite { envelope: envelope(s).unwrap_or_default(), ..GSite::default() };
    let found = conflicts(spec, p, &g.envelope, (0.0, 0.0));
    if !found.is_empty() {
        let text: Vec<String> = found.iter().map(SiteConflict::text).collect();
        return Err(GenError::Infeasible(format!("site rule conflict: {}", text.join("; "))));
    }
    if let (Some(plot), Some(st)) = (&s.plot, p.storeys.first()) {
        let a = area(plot);
        g.coverage = slab_area(&st.layout) / a;
        g.far = p.storeys.iter().map(|x| slab_area(&x.layout)).sum::<f64>() / a;
        p.metrics.extend([("coverage", g.coverage), ("floor_area_ratio", g.far)]);
    }
    if let Some(t) = &s.terrain {
        let surf = Surface::new(t)?;
        ground(spec, p, &surf, None, &mut g);
        let lock = s.lock_split;
        if lock.unwrap_or(g.slope > s.split_slope) {
            // the best way to part that keeps every room a barrier-free way to an entrance
            let found = split::candidates(spec, p, g.downhill).and_then(|list| {
                let mut why = String::new();
                for mut sp in list {
                    let mut q = p.clone();
                    if let Err(e) = split::provide(spec, &mut q, &mut sp) {
                        why = e;
                        continue;
                    }
                    (sp.plinth_cut, sp.plinth_fill) = (g.cut, g.fill);
                    let mut parted = GSite { envelope: g.envelope.clone(), coverage: g.coverage, far: g.far, ..GSite::default() };
                    ground(spec, &q, &surf, Some(&sp), &mut parted);
                    q.site = parted;
                    let lost = barrier_free(&q);
                    if lost.is_empty() {
                        return Ok(q);
                    }
                    why = format!("site.split_lift: no barrier-free way to an entrance from {}", lost.join(", "));
                }
                Err(why)
            });
            match found {
                Ok(mut q) => {
                    g = std::mem::take(&mut q.site);
                    *p = q;
                    if let Some(sp) = &g.split {
                        let count = |k: Access| sp.access.iter().filter(|a| a.kind == k).count() as f64;
                        p.metrics.extend([
                            ("split_drop", sp.drop),
                            ("split_flights", sp.flights.len() as f64),
                            ("split_cuts", sp.cuts.len() as f64),
                            ("split_platform_lifts", count(Access::Platform)),
                        ]);
                    }
                }
                Err(why) => {
                    if lock == Some(true) {
                        g.warnings.push(format!("site.locks.plinth split: {why}; the building stays on one level"));
                    }
                    g.split_why = why;
                }
            }
        } else {
            g.split_why = match lock {
                Some(_) => "site.locks.plinth keeps one level".into(),
                None => format!("the terrain falls {:.3} per m, under site.split_slope {}", g.slope, s.split_slope),
            };
        }
        let retained: f64 = g.walls.iter().map(|w| (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1)).sum();
        p.metrics.extend([
            ("ground_z", g.ground_z),
            ("cut_m3", g.cut),
            ("fill_m3", g.fill),
            ("excavation_m3", g.excavation),
            ("retaining_m", retained),
        ]);
    }
    p.site = g;
    Ok(())
}

/// The site findings of a built project: broken limits and doors with no way to grade are
/// failures; tall retaining walls are warnings.
pub(super) fn audit(spec: &ProjectSpec, p: &GProject) -> (Vec<String>, Vec<String>) {
    let mut fail: Vec<String> = conflicts(spec, p, &p.site.envelope, spec.origin).iter().map(SiteConflict::text).collect();
    for a in p.site.approaches.iter().filter(|a| a.kind.is_none()) {
        fail.push(format!(
            "site.entrance_step: the door at ({:.1}, {:.1}) is {:.2} m off grade with no room on the plot for a ramp or {:.1} m of steps",
            a.door.0, a.door.1, a.rise, a.run
        ));
    }
    if let Some(sp) = &p.site.split {
        split::audit(spec, p, sp, &mut fail);
    }
    (fail, p.site.warnings.clone())
}

/// Why the site came out as it did: rule, value, effect key and its slots.
pub(super) fn whys(spec: &ProjectSpec, p: &GProject) -> Vec<(&'static str, f64, &'static str, Args)> {
    let (s, g) = (&spec.site, &p.site);
    let mut out: Vec<(&'static str, f64, &'static str, Args)> = vec![];
    if !g.envelope.is_empty() {
        out.push(("site.plot_setback", s.uniform, "envelope", vec![
            ("area", format!("{:.0}", area(&g.envelope))), ("cover", format!("{:.3}", g.coverage)),
        ]));
        out.push(("site.coverage_max", s.coverage_max, "coverage", vec![
            ("cover", format!("{:.3}", g.coverage)), ("max", show(s.coverage_max)),
        ]));
        out.push(("site.far_max", s.far_max, "far", vec![("far", format!("{:.2}", g.far)), ("max", show(s.far_max))]));
        if !s.build_to.is_empty() {
            out.push(("site.build_to_share", rule("site.build_to_share"), "build_to", vec![("edges", format!("{:?}", s.build_to))]));
        }
    }
    if g.terrain {
        let idx = match s.ground {
            Ground::Balance => 0.0,
            Ground::Entrance => 1.0,
        };
        out.push(("site.ground_rule", idx, "ground", vec![
            ("z", format!("{:.2}", g.ground_z)), ("by", show(&g.ground_by)),
            ("cut", format!("{:.0}", g.cut)), ("fill", format!("{:.0}", g.fill)),
        ]));
        let terraces = g.walls.iter().map(|w| w.terrace + 1).max().unwrap_or(0);
        let slope = ("slope", format!("{:.3}", g.slope));
        out.push(if g.stepped {
            ("site.step_slope", s.step_slope, "step_stepped", vec![slope, ("n", show(terraces))])
        } else {
            ("site.step_slope", s.step_slope, "step_pad", vec![slope])
        });
        let high = g.walls.iter().map(|w| w.top - w.base).fold(0.0, f64::max);
        out.push(("site.retain_min", rule("site.retain_min"), "retain", vec![
            ("n", show(g.walls.len())), ("h", format!("{high:.2}")),
        ]));
        let n_of = |sp: &GSplit, k: Access| show(sp.access.iter().filter(|a| a.kind == k).count());
        out.push(match &g.split {
            Some(sp) => ("site.split_slope", s.split_slope, "split", vec![
                ("cuts", show(sp.cuts.len())),
                ("share", format!("{:.2}", sp.share)),
                ("drop", format!("{:.2}", sp.drop)),
                ("flights", show(sp.flights.len())),
                ("platform", n_of(sp, Access::Platform)),
                ("through", n_of(sp, Access::Through)),
                ("lifts", n_of(sp, Access::Lift)),
                ("moved", format!("{:.0}", g.cut + g.fill)),
                ("pad", format!("{:.0}", sp.plinth_cut + sp.plinth_fill)),
            ]),
            None => ("site.split_slope", s.split_slope, "one_level", vec![("why", show(&g.split_why))]),
        });
        let count = |k: Approach| show(g.approaches.iter().filter(|a| a.kind == Some(k)).count());
        out.push(("site.entrance_step", rule("site.entrance_step"), "exits", vec![
            ("n", show(g.approaches.len())),
            ("grade", count(Approach::Grade)),
            ("ramp", count(Approach::Ramp)),
            ("steps", count(Approach::Steps)),
        ]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_move_in_by_their_own_set_backs() {
        let plot = [(0.0, 0.0), (40.0, 0.0), (40.0, 30.0), (0.0, 30.0)];
        let r = inset_edges(&plot, &[5.0, 3.0, 8.0, 2.0]).unwrap();
        assert_eq!(r.len(), 4);
        let ((x0, y0), (x1, y1)) = footprint::bbox(&r);
        assert!((x0 - 2.0).abs() < 1e-9 && (y0 - 5.0).abs() < 1e-9, "{r:?}");
        assert!((x1 - 37.0).abs() < 1e-9 && (y1 - 22.0).abs() < 1e-9, "{r:?}");
        assert!(inset_edges(&plot, &[16.0, 0.0, 16.0, 0.0]).is_none());
    }

    #[test]
    fn a_plane_balances_at_its_mean_level() {
        let t = Terrain::Plane { z: 0.0, slope: 0.1, downhill: (1.0, 0.0) };
        let ring = [(0.0, 0.0), (20.0, 0.0), (20.0, 10.0), (0.0, 10.0)];
        let (cut, fill) = cut_fill(&ring, -1.0, &t).unwrap();
        // s * L * A / 8 each side of the mean
        assert!((cut - 50.0).abs() < 0.5 && (fill - 50.0).abs() < 0.5, "{cut} {fill}");
    }
}
