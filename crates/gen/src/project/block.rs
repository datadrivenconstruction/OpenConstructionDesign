//! A block of buildings on given plots. The shared site rules (set-backs to streets and to
//! neighbours, spacing, sun and fire access, the floor-area target and the programs) turn every
//! plot into one brief; the briefs are built in parallel by the same builder and gates as a
//! single project, then the block is checked as a whole. Every decision names its rule.
//! Coordinates are the block's frame in metres; each brief's `origin` places its plot.

use super::footprint::{self, P};
use super::rules::{def, rule, Layer, RuleSet};
use super::{build_project, check_project, merge_patch, parse_spec, GProject, Gates, ProjectSpec};
use ddc_geom::v2;
use serde_json::{json, Value as J};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use web_time::Instant;

/// One plot: its id, its outline and the brief fields only this plot changes.
#[derive(Clone, Debug, PartialEq)]
pub struct Plot {
    pub id: String,
    pub ring: Vec<P>,
    /// Merge patch on the shared brief for this plot.
    pub patch: J,
}

/// What a block request gives: plots, streets as areas and the shared brief with its rules.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockInput {
    pub plots: Vec<Plot>,
    pub streets: Vec<Vec<P>>,
    pub base: J,
}

/// How a plot edge meets its surroundings.
#[derive(Clone, Debug, PartialEq)]
pub enum Edge {
    Street,
    /// Shared with these plots.
    Shared(Vec<usize>),
    /// Neither: a park, a yard, open land.
    Free,
}

/// The brief one plot gets, and the decisions behind it.
#[derive(Clone, Debug, PartialEq)]
pub struct PlotPlan {
    pub id: String,
    /// Counter-clockwise, in the block's frame.
    pub ring: Vec<P>,
    pub edges: Vec<Edge>,
    pub setbacks: Vec<f64>,
    pub program: String,
    pub floors: u32,
    /// Height the spacing and sun rules count, m.
    pub height: f64,
    pub brief: J,
    pub why: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockPlan {
    pub plots: Vec<PlotPlan>,
    pub streets: Vec<Vec<P>>,
    pub spacing: f64,
    pub sun_ratio: f64,
    pub fire_access: f64,
    pub roof_allowance: f64,
}

/// One building as a single project has it.
#[derive(Clone, Debug)]
pub struct Built {
    pub spec: ProjectSpec,
    pub project: GProject,
    pub gates: Gates,
}

/// What the block check measured on one building.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    pub id: String,
    pub floors: usize,
    pub height: f64,
    pub footprint: f64,
    pub gfa: f64,
    /// Clear distance to the nearest street, m.
    pub street_gap: f64,
    /// Clear distance to the nearest other building, m (`f64::MAX` alone).
    pub nearest: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockReport {
    pub facts: Vec<Facts>,
    /// Broken block rules, each naming its rule; empty when the block holds.
    pub failures: Vec<String>,
    pub min_gap: f64,
}

/// A generated block: plan, buildings in plot order, the block check and the timings.
#[derive(Clone, Debug)]
pub struct Block {
    pub plan: BlockPlan,
    pub built: Vec<Built>,
    pub report: BlockReport,
    /// Plan, parallel build and check, ms.
    pub ms: [f64; 3],
    pub workers: usize,
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn tol() -> f64 {
    rule("site.tolerance")
}

fn area(r: &[P]) -> f64 {
    footprint::signed_area(r).abs()
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

fn cross(o: P, a: P, b: P) -> f64 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

fn segs_cross(a: P, b: P, c: P, d: P) -> bool {
    let (d1, d2) = (cross(c, d, a), cross(c, d, b));
    let (d3, d4) = (cross(a, b, c), cross(a, b, d));
    d1 * d2 < 0.0 && d3 * d4 < 0.0
}

/// Clear distance between two rings; 0 when they cross or one holds the other.
pub fn ring_gap(r: &[P], s: &[P]) -> f64 {
    if r.is_empty() || s.is_empty() {
        return f64::MAX;
    }
    if crate::check::inside(r, s[0]) || crate::check::inside(s, r[0]) {
        return 0.0;
    }
    let (n, m) = (r.len(), s.len());
    let mut best = f64::MAX;
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        for j in 0..m {
            let (c, d) = (s[j], s[(j + 1) % m]);
            if segs_cross(a, b, c, d) {
                return 0.0;
            }
            best = best.min(dist_seg(a, c, d)).min(dist_seg(b, c, d)).min(dist_seg(c, a, b)).min(dist_seg(d, a, b));
        }
    }
    best
}

/// Width of a ring: the least, over its edges, of the farthest vertex from the edge's line.
fn width(r: &[P]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| {
            let (a, b) = (r[i], r[(i + 1) % n]);
            let l = (b.0 - a.0).hypot(b.1 - a.1);
            if l <= 0.0 {
                return f64::MAX;
            }
            r.iter().map(|q| cross(a, b, *q).abs() / l).fold(0.0, f64::max)
        })
        .fold(f64::MAX, f64::min)
}

fn bbox_gap(a: &[P], b: &[P]) -> f64 {
    let ((ax0, ay0), (ax1, ay1)) = footprint::bbox(a);
    let ((bx0, by0), (bx1, by1)) = footprint::bbox(b);
    let dx = (bx0 - ax1).max(ax0 - bx1).max(0.0);
    let dy = (by0 - ay1).max(ay0 - by1).max(0.0);
    dx.hypot(dy)
}

fn points(v: &J, path: &str) -> Result<Vec<P>, String> {
    let a = v.as_array().ok_or(format!("{path}: a list of [x, y] in metres"))?;
    let mut out = Vec::with_capacity(a.len());
    for (i, q) in a.iter().enumerate() {
        match (q[0].as_f64(), q[1].as_f64()) {
            (Some(x), Some(y)) if x.is_finite() && y.is_finite() => out.push((x, y)),
            _ => return Err(format!("{path}[{i}]: [x, y] in metres")),
        }
    }
    let pts: Vec<_> = out.iter().map(|q| v2(q.0, q.1)).collect();
    if out.len() < 3 || ddc_geom::poly::simple_ring(&pts, tol()).is_err() || area(&out) <= tol() {
        return Err(format!("{path}: a simple ring of at least 3 points"));
    }
    if footprint::signed_area(&out) < 0.0 {
        out.reverse();
    }
    Ok(out)
}

/// The request as the tool and the command line give it: `plots` [{id, ring, overrides}],
/// `streets` [ring], `spec` the shared brief and `rules` the shared rules (merged into it).
pub fn read(args: &J) -> Result<BlockInput, String> {
    let list = args["plots"].as_array().filter(|a| !a.is_empty()).ok_or("plots: a list of {id, ring, overrides}")?;
    let mut plots: Vec<Plot> = Vec::with_capacity(list.len());
    for (i, p) in list.iter().enumerate() {
        let id = match &p["id"] {
            J::String(s) if !s.trim().is_empty() => s.trim().to_string(),
            J::Null => format!("{}", i + 1),
            other => other.to_string(),
        };
        if plots.iter().any(|q| q.id == id) {
            return Err(format!("plots[{i}]: id {id} twice"));
        }
        let patch = p["overrides"].clone();
        if !(patch.is_null() || patch.is_object()) {
            return Err(format!("plots[{i}].overrides: a merge patch on the shared spec"));
        }
        plots.push(Plot { id, ring: points(&p["ring"], &format!("plots[{i}].ring"))?, patch });
    }
    let mut streets = vec![];
    if let Some(a) = args["streets"].as_array() {
        for (i, s) in a.iter().enumerate() {
            streets.push(points(s, &format!("streets[{i}]"))?);
        }
    } else if !args["streets"].is_null() {
        return Err("streets: a list of rings".into());
    }
    let mut base = match &args["spec"] {
        J::Null => json!({}),
        J::Object(_) => args["spec"].clone(),
        _ => return Err("spec: the shared brief, an object".into()),
    };
    if !args["rules"].is_null() {
        if !args["rules"].is_object() {
            return Err("rules: {rule id: value}".into());
        }
        if !base["rules"].is_object() {
            base["rules"] = json!({});
        }
        merge_patch(&mut base["rules"], &args["rules"]);
    }
    Ok(BlockInput { plots, streets, base })
}

fn rules_of(patches: &[&J]) -> Result<RuleSet, String> {
    let mut rs = RuleSet::new();
    for p in patches.iter().filter(|p| !p.is_null()) {
        rs.apply_patch(Layer::Brief, p).map_err(|e| {
            e.iter().map(|x| format!("rules.{}: {}", x.rule, x.msg)).collect::<Vec<_>>().join("; ")
        })?;
    }
    Ok(rs)
}

fn option(rs: &RuleSet, id: &str) -> String {
    let d = def(id).unwrap_or_else(|| panic!("rule {id} is not in rules.json"));
    d.options.get(rs.get(id) as usize).cloned().unwrap_or_default()
}

/// Kind of every plot edge: on a street when its middle lies on or in a street area, shared
/// when it lies on another plot's edge.
fn edges_of(rings: &[Vec<P>], streets: &[Vec<P>], near: f64) -> Vec<Vec<Edge>> {
    rings
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let n = r.len();
            (0..n)
                .map(|e| {
                    let (a, b) = (r[e], r[(e + 1) % n]);
                    let mid = ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
                    if streets.iter().any(|s| crate::check::inside(s, mid) || dist_ring(mid, s) <= near) {
                        return Edge::Street;
                    }
                    let js: Vec<usize> =
                        (0..rings.len()).filter(|&j| j != i && dist_ring(mid, &rings[j]) <= near).collect();
                    if js.is_empty() { Edge::Free } else { Edge::Shared(js) }
                })
                .collect()
        })
        .collect()
}

/// Two street edges that are not parallel: the plot is on a corner.
fn corner(r: &[P], edges: &[Edge]) -> bool {
    let n = r.len();
    let dirs: Vec<P> = (0..n)
        .filter(|&e| edges[e] == Edge::Street)
        .map(|e| {
            let (a, b) = (r[e], r[(e + 1) % n]);
            let l = (b.0 - a.0).hypot(b.1 - a.1);
            ((b.0 - a.0) / l, (b.1 - a.1) / l)
        })
        .collect();
    dirs.iter().enumerate().any(|(k, u)| dirs[k + 1..].iter().any(|w| (u.0 * w.1 - u.1 * w.0).abs() > 0.5))
}

fn foot(p: P, a: P, b: P) -> P {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (a.0 + dx * t, a.1 + dy * t)
}

/// Nearest points of the segment a-b and the ring `r`: distance, point on the segment, on the ring.
fn nearest(a: P, b: P, r: &[P]) -> (f64, P, P) {
    let m = r.len();
    let mut best = (f64::MAX, a, a);
    for j in 0..m {
        let (c, d) = (r[j], r[(j + 1) % m]);
        for (p, q) in [(a, foot(a, c, d)), (b, foot(b, c, d)), (foot(c, a, b), c), (foot(d, a, b), d)] {
            let l = (q.0 - p.0).hypot(q.1 - p.1);
            if l < best.0 {
                best = (l, p, q);
            }
        }
    }
    best
}

/// Clear distance two buildings keep: the spacing, or the sun share of the taller one.
fn need_of(tall: f64, spacing: f64, sun: f64) -> (f64, String) {
    if sun * tall > spacing {
        (sun * tall, format!("block.sun_ratio {sun} x {tall:.1} m"))
    } else {
        (spacing, format!("block.spacing {spacing} m"))
    }
}

/// The site around plot `i`: every plot's ring and id.
struct Around<'a> {
    rings: &'a [Vec<P>],
    ids: &'a [&'a str],
}

/// Set-back of each edge of plot `i` for the heights `h`, with the rule behind each. A plot that
/// does not touch this one but stands closer than the clear distance pushes the edges that face
/// it back by their share of the rest, so the two buildings keep it whatever lies between.
fn setbacks(i: usize, edges: &[Edge], rs: &RuleSet, h: &[f64], spacing: f64, sun: f64, at: &Around<'_>) -> Vec<(f64, String)> {
    let side = rs.get("block.side_setback");
    let near = rs.get("block.street_tol");
    let ring = &at.rings[i];
    let n = ring.len();
    edges
        .iter()
        .enumerate()
        .map(|(k, e)| {
            let mut best = match e {
                Edge::Street => (rs.get("block.street_setback"), "block.street_setback".to_string()),
                Edge::Free => (side, "block.side_setback".to_string()),
                Edge::Shared(js) => {
                    let mut best = (side, "block.side_setback".to_string());
                    for &j in js {
                        let (need, why) = need_of(h[i].max(h[j]), spacing, sun);
                        if need * 0.5 > best.0 {
                            best = (need * 0.5, format!("half of {why}"));
                        }
                    }
                    best
                }
            };
            let (a, b) = (ring[k], ring[(k + 1) % n]);
            let l = (b.0 - a.0).hypot(b.1 - a.1);
            if l <= tol() {
                return best;
            }
            // counter-clockwise ring: the outside is on the right
            let out = ((b.1 - a.1) / l, -(b.0 - a.0) / l);
            for j in (0..at.rings.len()).filter(|&j| j != i) {
                let (d, p, q) = nearest(a, b, &at.rings[j]);
                if d <= near {
                    continue;
                }
                let facing = (out.0 * (q.0 - p.0) + out.1 * (q.1 - p.1)) / d;
                // an edge at more than 60 degrees to the neighbour barely moves away from it
                if facing < 0.5 {
                    continue;
                }
                let (need, why) = need_of(h[i].max(h[j]), spacing, sun);
                let s = (need - d) * 0.5 / facing;
                if s > best.0 {
                    best = (s, format!("half of {why} less the {d:.2} m to plot {}", at.ids[j]));
                }
            }
            best
        })
        .collect()
}

/// Two plots that share more than an edge or a corner.
fn overlap(r: &[P], s: &[P]) -> bool {
    let t = tol();
    let deep = |r: &[P], s: &[P]| {
        let k = s.len() as f64;
        let mid = (s.iter().map(|q| q.0).sum::<f64>() / k, s.iter().map(|q| q.1).sum::<f64>() / k);
        let own = crate::check::inside(s, mid).then_some(&mid);
        s.iter().chain(own).any(|q| crate::check::inside(r, *q) && dist_ring(*q, r) > t)
    };
    let (n, m) = (r.len(), s.len());
    deep(r, s)
        || deep(s, r)
        || (0..n).any(|i| (0..m).any(|j| segs_cross(r[i], r[(i + 1) % n], s[j], s[(j + 1) % m])))
}

/// The brief of one plot: the shared brief, the plot's patch, then what the rules decided where
/// neither names it. Plot rings go in relative to `origin`, the plot's south-west corner.
fn brief_of(base: &J, plot: &Plot, ring: &[P], program: Option<&str>, floors: Option<u32>, sb: &[f64]) -> J {
    let mut b = base.clone();
    merge_patch(&mut b, &plot.patch);
    let ((ox, oy), _) = footprint::bbox(ring);
    if let Some(p) = program {
        b["typology"] = json!(p);
    }
    if let Some(f) = floors {
        b["floors"] = json!(f);
    }
    if b["name"].is_null() {
        b["name"] = json!(plot.id);
    }
    b["origin"] = json!([ox, oy]);
    let local: Vec<J> = ring.iter().map(|q| json!([q.0 - ox, q.1 - oy])).collect();
    b["footprint"] = json!({ "plot": local });
    if !b["site"].is_object() {
        b["site"] = json!({});
    }
    b["site"]["plot"] = J::Null;
    if plot.patch["site"]["setbacks"].is_null() {
        b["site"]["setbacks"] = json!(sb);
    }
    // a null left by the patch is not a field
    if let Some(o) = b["site"].as_object_mut() {
        o.retain(|_, v| !v.is_null());
    }
    b
}

/// One brief per plot from the shared rules. Storeys follow the floor-area target; where a tall
/// neighbour's spacing leaves a plot narrower than `block.min_width`, the tallest building
/// next to it loses a storey until every plot fits or none can give one.
pub fn plan(input: &BlockInput) -> Result<BlockPlan, String> {
    let shared = rules_of(&[&input.base["rules"]])?;
    let spacing = shared.get("block.spacing");
    let sun = shared.get("block.sun_ratio");
    let near = shared.get("block.street_tol");
    let allowance = shared.get("block.roof_allowance");
    let n = input.plots.len();
    let mut rs = Vec::with_capacity(n);
    for p in &input.plots {
        rs.push(rules_of(&[&input.base["rules"], &p.patch["rules"]]).map_err(|e| format!("plot {}: {e}", p.id))?);
    }
    let rings: Vec<Vec<P>> = input.plots.iter().map(|p| p.ring.clone()).collect();
    for i in 0..n {
        for j in i + 1..n {
            if overlap(&rings[i], &rings[j]) {
                return Err(format!("plots {} and {} overlap: every building needs its own plot", input.plots[i].id, input.plots[j].id));
            }
        }
    }
    let ids: Vec<&str> = input.plots.iter().map(|p| p.id.as_str()).collect();
    let at = Around { rings: &rings, ids: &ids };
    let edges = edges_of(&rings, &input.streets, near);
    let mut why: Vec<Vec<String>> = vec![vec![]; n];
    // the program: named by the brief or the plot, else by the street rule
    let mut program: Vec<Option<String>> = vec![None; n];
    let mut fixed_floors: Vec<bool> = vec![false; n];
    for i in 0..n {
        let mut merged = input.base.clone();
        merge_patch(&mut merged, &input.plots[i].patch);
        fixed_floors[i] = merged["floors"].is_number();
        if merged["typology"].is_null() && merged["preset"].is_null() {
            let (id, how) = if corner(&rings[i], &edges[i]) {
                ("block.corner_program", "on two streets that meet")
            } else {
                ("block.street_program", "not on a corner")
            };
            let p = option(&rs[i], id);
            why[i].push(format!("program {p}: {how} ({id})"));
            program[i] = Some(p);
        }
        if !edges[i].contains(&Edge::Street) {
            why[i].push("no edge on a street: reached across the neighbours (block.fire_access)".into());
        }
    }
    // floor heights and fixed storeys from the brief as parsed, spacing-only set-backs first
    let zero = vec![0.0; n];
    let mut floors: Vec<u32> = vec![0; n];
    let mut fh: Vec<f64> = vec![0.0; n];
    for i in 0..n {
        let sb: Vec<f64> = setbacks(i, &edges[i], &rs[i], &zero, spacing, sun, &at).into_iter().map(|s| s.0).collect();
        let env = super::site::inset_edges(&rings[i], &sb)
            .ok_or_else(|| format!("plot {}: the set-backs leave nothing of it (block.street_setback, block.spacing)", input.plots[i].id))?;
        let lo = rs[i].get("block.storeys_min") as u32;
        let hi = (rs[i].get("block.storeys_max") as u32).max(lo);
        let far = rs[i].get("block.far_target");
        let want = (far * area(&rings[i]) / area(&env)).round().max(1.0) as u32;
        let f = want.clamp(lo, hi);
        let probe = brief_of(&input.base, &input.plots[i], &rings[i], program[i].as_deref(), (!fixed_floors[i]).then_some(f), &sb);
        let spec = parse_spec(&probe).map_err(|e| format!("plot {}: {e}", input.plots[i].id))?;
        floors[i] = spec.floors;
        // mean storey: tall levels count in the plot height
        fh[i] = spec.building_height() / f64::from(spec.floors.max(1));
        if !fixed_floors[i] {
            why[i].push(format!(
                "{f} storeys: block.far_target {far} x plot {:.0} m2 / buildable {:.0} m2, within block.storeys_min {lo} .. block.storeys_max {hi}",
                area(&rings[i]),
                area(&env)
            ));
        }
    }
    let lo: Vec<u32> = rs.iter().map(|r| r.get("block.storeys_min") as u32).collect();
    let heights = |floors: &[u32]| -> Vec<f64> { (0..n).map(|i| f64::from(floors[i]) * fh[i] + allowance).collect() };
    // plots that share an edge, and plots near enough for the clear distance to reach across
    let neighbours = |i: usize, h: &[f64]| -> Vec<usize> {
        let mut js: Vec<usize> = edges[i].iter().flat_map(|e| if let Edge::Shared(js) = e { js.clone() } else { vec![] }).collect();
        js.extend((0..n).filter(|&j| j != i && ring_gap(&rings[i], &rings[j]) < need_of(h[i].max(h[j]), spacing, sun).0));
        js.sort_unstable();
        js.dedup();
        js
    };
    let sb_all = loop {
        let h = heights(&floors);
        let sb_all: Vec<Vec<(f64, String)>> = (0..n).map(|i| setbacks(i, &edges[i], &rs[i], &h, spacing, sun, &at)).collect();
        let env_all: Vec<Option<Vec<P>>> = (0..n)
            .map(|i| super::site::inset_edges(&rings[i], &sb_all[i].iter().map(|s| s.0).collect::<Vec<_>>()))
            .collect();
        let bad: Vec<usize> = (0..n)
            .filter(|&i| env_all[i].as_ref().is_none_or(|e| width(e) + tol() < rs[i].get("block.min_width")))
            .collect();
        if bad.is_empty() {
            break sb_all;
        }
        let mut lowered = false;
        for &i in &bad {
            let pick = std::iter::once(i)
                .chain(neighbours(i, &h))
                .filter(|&k| !fixed_floors[k] && floors[k] > lo[k])
                .max_by(|a, b| h[*a].total_cmp(&h[*b]).then(b.cmp(a)));
            if let Some(k) = pick {
                if heights(&floors)[k] < h[k] {
                    continue;
                }
                floors[k] -= 1;
                lowered = true;
                let w = env_all[i].as_ref().map_or(0.0, |e| width(e));
                why[k].push(format!(
                    "{} storeys: plot {} is {w:.1} m wide inside its set-backs, block.min_width asks {} (block.sun_ratio)",
                    floors[k],
                    input.plots[i].id,
                    rs[i].get("block.min_width")
                ));
            }
        }
        if !lowered {
            let names: Vec<String> = bad
                .iter()
                .map(|&i| {
                    let kept: Vec<String> = std::iter::once(i)
                        .chain(neighbours(i, &h))
                        .filter(|&k| fixed_floors[k])
                        .map(|k| format!("{} {} storeys", input.plots[k].id, floors[k]))
                        .collect();
                    let kept = if kept.is_empty() { String::new() } else { format!(", storeys the brief asks are kept ({})", kept.join(", ")) };
                    format!("plot {}: narrower than block.min_width {} inside its set-backs{kept}", input.plots[i].id, rs[i].get("block.min_width"))
                })
                .collect();
            return Err(format!(
                "{} (set-backs from block.street_setback, block.side_setback, block.spacing, block.sun_ratio)",
                names.join("; ")
            ));
        }
    };
    let h = heights(&floors);
    let mut plots = Vec::with_capacity(n);
    for i in 0..n {
        let sb: Vec<f64> = sb_all[i].iter().map(|s| s.0).collect();
        for (e, s) in sb_all[i].iter().enumerate() {
            why[i].push(format!("edge {e}: set back {:.2} m ({})", s.0, s.1));
        }
        let brief = brief_of(&input.base, &input.plots[i], &rings[i], program[i].as_deref(), (!fixed_floors[i]).then_some(floors[i]), &sb);
        plots.push(PlotPlan {
            id: input.plots[i].id.clone(),
            ring: rings[i].clone(),
            edges: edges[i].clone(),
            setbacks: sb,
            program: program[i].clone().unwrap_or_else(|| brief["typology"].as_str().or(brief["preset"].as_str()).unwrap_or_default().to_string()),
            floors: floors[i],
            height: h[i],
            brief,
            why: std::mem::take(&mut why[i]),
        });
    }
    Ok(BlockPlan {
        plots,
        streets: input.streets.clone(),
        spacing,
        sun_ratio: sun,
        fire_access: shared.get("block.fire_access"),
        roof_allowance: allowance,
    })
}

/// One brief as a single project: spec, build, gates; a failure is the text to fix it by.
pub fn build_one(brief: &J) -> Result<Built, String> {
    let spec = parse_spec(brief).map_err(|e| e.to_string())?;
    let project = build_project(&spec).map_err(|e| e.to_string())?;
    let gates = check_project(&spec, &project);
    if !gates.passed() {
        return Err(format!("the project fails its gates: {}", gates.failures.join("; ")));
    }
    Ok(Built { spec, project, gates })
}

/// Threads the machine offers.
pub fn workers() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// Every brief on up to `workers` threads, results in brief order whatever the timing.
pub fn build_all(briefs: &[J], workers: usize) -> Vec<Result<Built, String>> {
    let results: Mutex<Vec<Option<Result<Built, String>>>> = Mutex::new(vec![None; briefs.len()]);
    let next = AtomicUsize::new(0);
    let w = workers.clamp(1, briefs.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..w {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(b) = briefs.get(i) else { break };
                let r = build_one(b);
                results.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_else(|e| e.into_inner())
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err("not built".to_string())))
        .collect()
}

/// Above-ground outlines of a building: every storey's slab and every roof.
fn outlines(p: &GProject) -> Vec<&[P]> {
    p.storeys
        .iter()
        .map(|s| s.layout.slab.as_slice())
        .chain(std::iter::once(p.roof.ring.as_slice()))
        .chain(p.extra_roofs.iter().map(|r| r.ring.as_slice()))
        .filter(|r| r.len() >= 3)
        .collect()
}

/// Height the spacing and sun rules count: the roof level, the allowance and a pitched roof's
/// rise over half the roof's width.
pub fn height(p: &GProject, allowance: f64) -> f64 {
    let rise = std::iter::once(&p.roof)
        .chain(&p.extra_roofs)
        .filter(|r| r.slope > super::FLAT_FALL_DEG + tol() && r.ring.len() >= 3)
        .map(|r| r.slope.to_radians().tan() * width(&r.ring) * 0.5)
        .fold(0.0, f64::max);
    p.roof.elevation + allowance + rise
}

/// The block as a whole: every building inside its plot, clear of every other by the spacing
/// and sun rules, and within reach of a street.
pub fn check(plan: &BlockPlan, built: &[&GProject]) -> BlockReport {
    let tol = tol();
    let mut out = BlockReport { min_gap: f64::MAX, ..BlockReport::default() };
    let rings: Vec<Vec<&[P]>> = built.iter().map(|p| outlines(p)).collect();
    let hs: Vec<f64> = built.iter().map(|p| height(p, plan.roof_allowance)).collect();
    let mut nearest = vec![f64::MAX; built.len()];
    for (i, pp) in plan.plots.iter().enumerate() {
        let out_of = rings[i].iter().flat_map(|r| r.iter()).any(|q| !crate::check::inside(&pp.ring, *q) && dist_ring(*q, &pp.ring) > tol);
        if out_of {
            out.failures.push(format!("plot {}: the building leaves its plot", pp.id));
        }
    }
    for i in 0..built.len() {
        for j in i + 1..built.len() {
            let tall = hs[i].max(hs[j]);
            let need = plan.spacing.max(plan.sun_ratio * tall);
            let ground = (rings[i].first(), rings[j].first());
            if let (Some(a), Some(b)) = ground {
                if bbox_gap(a, b) > need + spread(&rings[i]) + spread(&rings[j]) {
                    continue;
                }
            }
            let gap = rings[i].iter().flat_map(|a| rings[j].iter().map(move |b| ring_gap(a, b))).fold(f64::MAX, f64::min);
            nearest[i] = nearest[i].min(gap);
            nearest[j] = nearest[j].min(gap);
            out.min_gap = out.min_gap.min(gap);
            let (a, b) = (&plan.plots[i].id, &plan.plots[j].id);
            if gap <= tol {
                out.failures.push(format!("plots {a} and {b}: the buildings overlap"));
            } else if gap + tol < plan.spacing {
                out.failures.push(format!("plots {a} and {b}: {gap:.2} m apart, block.spacing asks {}", plan.spacing));
            } else if gap + tol < need {
                out.failures.push(format!(
                    "plots {a} and {b}: {gap:.2} m apart, block.sun_ratio {} x {tall:.1} m asks {need:.2}",
                    plan.sun_ratio
                ));
            }
        }
    }
    for (i, p) in built.iter().enumerate() {
        let ground = rings[i].first().copied().unwrap_or(&[]);
        let street_gap = plan.streets.iter().map(|s| ring_gap(ground, s)).fold(f64::MAX, f64::min);
        if street_gap > plan.fire_access + tol {
            let d = if street_gap == f64::MAX { "no street given".to_string() } else { format!("{street_gap:.1} m from a street") };
            out.failures.push(format!("plot {}: {d}, block.fire_access asks at most {}", plan.plots[i].id, plan.fire_access));
        }
        let gfa: f64 = p.storeys.iter().map(|s| area(&s.layout.slab)).sum();
        out.facts.push(Facts {
            id: plan.plots[i].id.clone(),
            floors: p.storeys.len(),
            height: hs[i],
            footprint: area(ground),
            gfa,
            street_gap,
            nearest: nearest[i],
        });
    }
    out
}

/// What a building owes the buildings already on the site, outside any block: the clear
/// distance by `block.spacing` and `block.sun_ratio` of the brief's rules.
#[derive(Clone, Debug, PartialEq)]
pub struct Clearance {
    /// Clear distance between the outlines, m; 0 when they overlap.
    pub gap: f64,
    /// What the rules ask, m, and the rule that sets it.
    pub need: f64,
    pub rule: String,
}

impl Clearance {
    pub fn overlaps(&self) -> bool {
        self.gap <= tol()
    }

    pub fn holds(&self) -> bool {
        self.gap + tol() >= self.need
    }
}

/// Clearance of building `a` from building `b` under the rules of `a`'s brief.
pub fn clearance(a: &GProject, b: &GProject, rules: &J) -> Result<Clearance, String> {
    let rs = rules_of(&[rules])?;
    let allowance = rs.get("block.roof_allowance");
    let (ra, rb) = (outlines(a), outlines(b));
    let gap = ra.iter().flat_map(|x| rb.iter().map(move |y| ring_gap(x, y))).fold(f64::MAX, f64::min);
    let tall = height(a, allowance).max(height(b, allowance));
    let (need, rule) = need_of(tall, rs.get("block.spacing"), rs.get("block.sun_ratio"));
    Ok(Clearance { gap, need, rule })
}

/// Box of a building's outlines, m.
pub fn extent(p: &GProject) -> Option<(P, P)> {
    let all: Vec<P> = outlines(p).into_iter().flat_map(|r| r.iter().copied()).collect();
    (!all.is_empty()).then(|| footprint::bbox(&all))
}

/// How far a building's outlines reach past its ground floor's box: upper floors and roofs.
fn spread(rings: &[&[P]]) -> f64 {
    let Some(g) = rings.first() else { return 0.0 };
    let ((x0, y0), (x1, y1)) = footprint::bbox(g);
    rings
        .iter()
        .flat_map(|r| r.iter())
        .map(|q| (x0 - q.0).max(q.0 - x1).max(y0 - q.1).max(q.1 - y1).max(0.0))
        .fold(0.0, f64::max)
}

/// Plan, parallel build and block check. A plot that does not build refuses the block with
/// every plot's reason; the block check's failures are in the report.
pub fn generate(input: &BlockInput, workers: usize) -> Result<Block, String> {
    let t = Instant::now();
    let plan = plan(input)?;
    let t_plan = ms(t);
    let t = Instant::now();
    let briefs: Vec<J> = plan.plots.iter().map(|p| p.brief.clone()).collect();
    let results = build_all(&briefs, workers);
    let t_build = ms(t);
    let mut built = Vec::with_capacity(results.len());
    let mut errs = vec![];
    for (p, r) in plan.plots.iter().zip(results) {
        match r {
            Ok(b) => built.push(b),
            Err(e) => errs.push(format!("plot {}: {e}", p.id)),
        }
    }
    if !errs.is_empty() {
        return Err(errs.join("\n"));
    }
    let t = Instant::now();
    let report = check(&plan, &built.iter().map(|b| &b.project).collect::<Vec<_>>());
    let t_check = ms(t);
    Ok(Block { plan, built, report, ms: [t_plan, t_build, t_check], workers: workers.clamp(1, briefs.len().max(1)) })
}

impl BlockReport {
    /// What the assistant hears about the block.
    pub fn to_json(&self) -> J {
        let r = |x: f64| if x == f64::MAX { J::Null } else { json!((x * 1000.0).round() / 1000.0) };
        json!({
            "passed": self.failures.is_empty(),
            "failures": self.failures,
            "min_gap_m": r(self.min_gap),
            "buildings": self.facts.iter().map(|f| json!({
                "plot": f.id, "floors": f.floors, "height_m": r(f.height), "footprint_m2": r(f.footprint),
                "gfa_m2": r(f.gfa), "street_gap_m": r(f.street_gap), "nearest_m": r(f.nearest),
            })).collect::<Vec<_>>(),
        })
    }
}
