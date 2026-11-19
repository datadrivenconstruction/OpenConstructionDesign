// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Regression benchmark of the project generator: the briefs of `data/bench/*.json`, each built
//! by `build_project` and measured on the built model only, never on what the generator says
//! about itself. Shared by `tests/bench.rs` (the gate against `bench/baseline.json`) and
//! `examples/gen_bench.rs` (the table). Thresholds are fixed here, not read from the rule set,
//! so changing a rule cannot move its own score.
#![allow(dead_code)]

use ddc_gen::project::why::explain_project;
use ddc_gen::project::{
    build_project, check_project, parse_spec, GProject, PRoom, PStorey, ProjectSpec, RoomKind,
    Typology,
};
use ddc_gen::{GLayout, GOpening, GRoom, GWall, WallRole};
use serde_json::{json, Map, Value as J};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub type P = (f64, f64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Better {
    Higher,
    Lower,
}

pub struct Metric {
    pub id: &'static str,
    /// Column head of the table, at most seven characters.
    pub short: &'static str,
    pub unit: &'static str,
    pub better: Better,
    /// Largest worsening of one brief that still passes; 0 for counts and pass/fail measures.
    pub tol: f64,
}

const fn metric(id: &'static str, short: &'static str, unit: &'static str, better: Better, tol: f64) -> Metric {
    Metric { id, short, unit, better, tol }
}

pub const RUNTIME: &str = "runtime_ms";

pub const METRICS: [Metric; 18] = [
    metric("area_efficiency", "eff", "% net/gross", Better::Higher, 1.0),
    metric("programme_error", "prg_err", "% mean area error", Better::Lower, 1.0),
    metric("adjacency", "adj", "% weighted wishes met", Better::Higher, 1.0),
    metric("circulation_ratio", "circ", "% of room area", Better::Lower, 1.0),
    metric("daylight", "dayl", "% lit rooms passing", Better::Higher, 1.0),
    metric("egress_max", "egr_m", "m to an exit", Better::Lower, 1.0),
    metric("egress_reachable", "egr_ok", "% rooms with a way out", Better::Higher, 0.5),
    metric("wet_stacking", "wet", "% wet rooms stacked", Better::Higher, 1.0),
    metric("grid_regularity", "grid", "% span uniformity", Better::Higher, 1.0),
    metric("column_continuity", "cols", "% columns carried", Better::Higher, 0.5),
    metric("risers_per_1000m2", "risers", "risers / 1000 m2", Better::Lower, 0.25),
    metric("norm_findings", "norms", "checker findings", Better::Lower, 0.0),
    metric("hard_failures", "hard", "checker failures", Better::Lower, 0.0),
    metric("clashes", "clash", "geometric clashes", Better::Lower, 0.0),
    metric("site_fit", "site", "% slab corners on plot", Better::Higher, 0.5),
    metric("deterministic", "det", "% identical rebuild", Better::Higher, 0.0),
    metric("why_coverage", "why", "% elements explained", Better::Higher, 1.0),
    metric(RUNTIME, "ms", "build ms (total gated)", Better::Lower, 0.0),
];

/// The suite total may grow this many times plus the slack before the gate fails: timings run
/// on a shared machine, unoptimised in tests.
pub const RUNTIME_FACTOR: f64 = 3.0;
pub const RUNTIME_SLACK_MS: f64 = 2000.0;

/// A point this close to a room outline is on it (openings sit on wall centre lines).
const ON_ROOM: f64 = 0.05;
/// Shortest shared wall that makes two rooms neighbours: room for a door.
const MIN_SHARED: f64 = 0.9;
/// Glazing per floor area and the lit depth per window head height of the daylight proxy.
const GLAZING_SHARE: f64 = 1.0 / 8.0;
const DEPTH_PER_HEAD: f64 = 2.5;
/// Wet rooms whose centres lie this close share a riser.
const STACK_REACH: f64 = 1.5;

#[derive(Clone, Debug)]
pub struct Brief {
    pub name: String,
    pub group: String,
    pub spec: J,
    /// Wished neighbours: kind, kind, weight.
    pub adjacency: Vec<(RoomKind, RoomKind, f64)>,
    pub plot: Option<Vec<P>>,
    /// Terrain fall per metre and the downhill direction.
    pub slope: Option<(f64, P)>,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub name: String,
    pub group: String,
    pub refused: Option<String>,
    pub metrics: BTreeMap<&'static str, Option<f64>>,
    pub detail: J,
}

impl Row {
    pub fn get(&self, id: &str) -> Option<f64> {
        self.metrics.get(id).copied().flatten()
    }
}

pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn baseline_path() -> PathBuf {
    crate_dir().join("bench/baseline.json")
}

pub fn kind_of(s: &str) -> Option<RoomKind> {
    RoomKind::parse(s).or(match s {
        "corridor" => Some(RoomKind::Corridor),
        "stair" => Some(RoomKind::Stair),
        "lift" => Some(RoomKind::Lift),
        _ => None,
    })
}

fn points(v: &J) -> Option<Vec<P>> {
    v.as_array()?
        .iter()
        .map(|q| Some((q.get(0)?.as_f64()?, q.get(1)?.as_f64()?)))
        .collect()
}

pub fn read_brief(f: &Path) -> Brief {
    let name = f.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
    let text = std::fs::read_to_string(f).unwrap_or_else(|e| panic!("{name}: {e}"));
    let v: J = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(v["schema"], "gen_bench.v1", "{name}: schema");
    assert!(v["spec"].is_object(), "{name}: spec must be an object");
    let adjacency = v["adjacency"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|w| {
                    let k = |i: usize| {
                        w[i].as_str()
                            .and_then(kind_of)
                            .unwrap_or_else(|| panic!("{name}: adjacency {w}: unknown kind"))
                    };
                    let weight = w[2].as_f64().unwrap_or_else(|| panic!("{name}: adjacency {w}: weight"));
                    (k(0), k(1), weight)
                })
                .collect()
        })
        .unwrap_or_default();
    let site = &v["site"];
    let plot = (!site["plot"].is_null())
        .then(|| points(&site["plot"]).unwrap_or_else(|| panic!("{name}: site.plot")));
    let down = (
        site["downhill"][0].as_f64().unwrap_or(0.0),
        site["downhill"][1].as_f64().unwrap_or(-1.0),
    );
    Brief {
        group: v["group"].as_str().unwrap_or_else(|| panic!("{name}: group")).to_string(),
        spec: v["spec"].clone(),
        adjacency,
        plot,
        slope: site["slope"].as_f64().map(|s| (s, down)),
        name,
    }
}

/// The brief's spec with the bench terrain as `site.terrain` when the spec gives none, so a
/// steep brief is built on its slope.
pub fn spec_of(b: &Brief) -> J {
    let mut v = b.spec.clone();
    if let Some((s, d)) = b.slope {
        if v.is_object() && v["site"]["terrain"].is_null() {
            v["site"]["terrain"] = json!({"slope": s, "downhill": [d.0, d.1]});
        }
    }
    v
}

/// Every brief of `data/bench`, in file name order.
pub fn load_briefs() -> Vec<Brief> {
    let dir = crate_dir().join("data/bench");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files.iter().map(|f| read_brief(f)).collect()
}

// ---- geometry ----

pub fn area(poly: &[P]) -> f64 {
    let n = poly.len();
    let s: f64 = (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    s.abs() / 2.0
}

fn hyp(a: P, b: P) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

pub fn dist_seg(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
    };
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

pub fn dist_poly(p: P, poly: &[P]) -> f64 {
    let n = poly.len();
    (0..n)
        .map(|i| dist_seg(p, poly[i], poly[(i + 1) % n]))
        .fold(f64::MAX, f64::min)
}

/// Even-odd point in polygon.
pub fn inside(p: P, poly: &[P]) -> bool {
    let n = poly.len();
    let mut c = false;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) * (b.0 - a.0) / (b.1 - a.1) {
            c = !c;
        }
    }
    c
}

fn centroid(poly: &[P]) -> P {
    let n = poly.len().max(1) as f64;
    (
        poly.iter().map(|p| p.0).sum::<f64>() / n,
        poly.iter().map(|p| p.1).sum::<f64>() / n,
    )
}

fn bbox(poly: &[P]) -> (P, P) {
    let mut lo = (f64::MAX, f64::MAX);
    let mut hi = (f64::MIN, f64::MIN);
    for p in poly {
        lo = (lo.0.min(p.0), lo.1.min(p.1));
        hi = (hi.0.max(p.0), hi.1.max(p.1));
    }
    (lo, hi)
}

fn opening_centre(w: &GWall, o: &GOpening) -> P {
    let (dx, dy) = (w.b.0 - w.a.0, w.b.1 - w.a.1);
    let l = dx.hypot(dy).max(1e-9);
    let d = o.along + o.width / 2.0;
    (w.a.0 + dx / l * d, w.a.1 + dy / l * d)
}

/// Two segments cross inside both: an end within 1 cm of the other segment is a junction.
pub fn crosses(a: P, b: P, c: P, d: P) -> bool {
    let r = (b.0 - a.0, b.1 - a.1);
    let s = (d.0 - c.0, d.1 - c.1);
    let (lr, ls) = (r.0.hypot(r.1), s.0.hypot(s.1));
    if lr < 1e-9 || ls < 1e-9 {
        return false;
    }
    let den = r.0 * s.1 - r.1 * s.0;
    if den.abs() < 1e-9 * lr * ls {
        return false;
    }
    let q = (c.0 - a.0, c.1 - a.1);
    let t = (q.0 * s.1 - q.1 * s.0) / den;
    let u = (q.0 * r.1 - q.1 * r.0) / den;
    let (et, eu) = (0.01 / lr, 0.01 / ls);
    t > et && t < 1.0 - et && u > eu && u < 1.0 - eu
}

/// Length along which segment `r s` lies on segment `p q` (2 cm off the line at most).
pub fn seg_overlap(p: P, q: P, r: P, s: P) -> f64 {
    let d = (q.0 - p.0, q.1 - p.1);
    let l = d.0.hypot(d.1);
    if l < 1e-9 {
        return 0.0;
    }
    let u = (d.0 / l, d.1 / l);
    let off = |x: P| ((x.0 - p.0) * u.1 - (x.1 - p.1) * u.0).abs();
    if off(r) > 0.02 || off(s) > 0.02 {
        return 0.0;
    }
    let t = |x: P| (x.0 - p.0) * u.0 + (x.1 - p.1) * u.1;
    let (t0, t1) = (t(r).min(t(s)), t(r).max(t(s)));
    (t1.min(l) - t0.max(0.0)).max(0.0)
}

/// Total length of the outline two polygons share.
pub fn shared_edge(a: &[P], b: &[P]) -> f64 {
    let (na, nb) = (a.len(), b.len());
    let mut len = 0.0;
    for i in 0..na {
        for j in 0..nb {
            len += seg_overlap(a[i], a[(i + 1) % na], b[j], b[(j + 1) % nb]);
        }
    }
    len
}

// ---- the model ----

fn all_storeys(p: &GProject) -> impl Iterator<Item = &PStorey> + '_ {
    p.storeys.iter().chain(&p.basements)
}

fn rooms(st: &PStorey) -> impl Iterator<Item = (&GRoom, &PRoom)> + '_ {
    st.layout.rooms.iter().zip(&st.rooms)
}

fn is_circ(k: RoomKind) -> bool {
    k.circulation() || k == RoomKind::Stair || k == RoomKind::Lift
}

fn pct(ok: usize, all: usize) -> Option<f64> {
    (all > 0).then(|| 100.0 * ok as f64 / all as f64)
}

fn slab_area(l: &GLayout) -> f64 {
    area(&l.slab) - l.slab_holes.iter().map(|h| area(h)).sum::<f64>()
}

/// Gross floor area: every slab, below ground too, less its holes.
pub fn gross(p: &GProject) -> f64 {
    all_storeys(p).map(|s| slab_area(&s.layout)).sum()
}

fn area_efficiency(p: &GProject, gross: f64) -> Option<f64> {
    let net: f64 = all_storeys(p)
        .flat_map(|s| rooms(s))
        .filter(|(_, r)| !is_circ(r.kind))
        .map(|(g, _)| area(&g.polygon))
        .sum();
    (gross > 0.0).then(|| 100.0 * net / gross)
}

fn circulation_ratio(p: &GProject) -> Option<f64> {
    let (mut circ, mut total) = (0.0, 0.0);
    for (g, r) in all_storeys(p).flat_map(|s| rooms(s)) {
        let a = area(&g.polygon);
        total += a;
        if is_circ(r.kind) {
            circ += a;
        }
    }
    (total > 0.0).then(|| 100.0 * circ / total)
}

/// Rooms the brief asks on storey `k`: kind and area, one entry per room of every wing.
fn requests(s: &ProjectSpec, k: usize, copies: u32) -> Vec<(RoomKind, f64)> {
    let mut list: Vec<(RoomKind, f64, u32)> = vec![];
    if s.typology == Typology::ApartmentSection {
        for a in &s.apartments {
            for r in a.rooms.iter().filter(|r| r.kind != RoomKind::Hall) {
                list.push((r.kind, r.area, r.count * a.count));
            }
        }
    } else {
        for r in s.floor_rooms(k) {
            list.push((r.kind, r.area, r.count));
        }
    }
    let mut out = vec![];
    for (kind, a, n) in list {
        for _ in 0..n * copies.max(1) {
            out.push((kind, a));
        }
    }
    out
}

/// Mean relative area error of the asked rooms against the measured ones of the same kind on
/// the same storey, largest asks matched first; a missing room counts as a full miss.
fn programme_error(s: &ProjectSpec, p: &GProject) -> (Option<f64>, usize) {
    let (mut err, mut n, mut missing) = (0.0, 0usize, 0usize);
    for (k, st) in p.storeys.iter().enumerate() {
        let mut want = requests(s, k, p.copies);
        want.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut pool: Vec<(RoomKind, f64, bool)> =
            rooms(st).map(|(g, r)| (r.kind, area(&g.polygon), false)).collect();
        for (kind, a) in want {
            n += 1;
            let best = pool
                .iter_mut()
                .filter(|x| x.0 == kind && !x.2)
                .min_by(|x, y| (x.1 - a).abs().total_cmp(&(y.1 - a).abs()));
            match best {
                Some(x) => {
                    x.2 = true;
                    err += ((x.1 - a).abs() / a.max(1e-9)).min(1.0);
                }
                None => {
                    err += 1.0;
                    missing += 1;
                }
            }
        }
    }
    ((n > 0).then(|| 100.0 * err / n as f64), missing)
}

/// Wishes of the bench file and of the spec's programme (`adjacent_to`).
fn wishes(s: &ProjectSpec, b: &Brief) -> Vec<(RoomKind, RoomKind, f64)> {
    let mut out = b.adjacency.clone();
    for sp in &s.program.spaces {
        for (name, w) in &sp.adjacent_to {
            let to = s
                .program
                .spaces
                .iter()
                .find(|t| t.answers(name))
                .map(|t| t.kind)
                .or_else(|| kind_of(name));
            if let Some(to) = to {
                out.push((sp.kind, to, *w));
            }
        }
    }
    out
}

/// Weighted share of wishes met: for each wish, the share of rooms of the first kind sharing at
/// least a door width of wall with a room of the second kind on the same storey (and in the
/// same apartment when both belong to one).
fn adjacency(p: &GProject, wishes: &[(RoomKind, RoomKind, f64)]) -> Option<f64> {
    let (mut got, mut all) = (0.0, 0.0);
    for &(a, b, w) in wishes {
        let (mut n, mut ok) = (0usize, 0usize);
        for st in all_storeys(p) {
            let list: Vec<(&GRoom, &PRoom)> = rooms(st).collect();
            for (ga, _) in list.iter().filter(|(_, r)| r.kind == a) {
                n += 1;
                let next = list.iter().any(|(gb, rb)| {
                    rb.kind == b
                        && !std::ptr::eq(*ga, *gb)
                        && (ga.apartment.is_none() || gb.apartment.is_none() || ga.apartment == gb.apartment)
                        && shared_edge(&ga.polygon, &gb.polygon) >= MIN_SHARED - 1e-6
                });
                ok += usize::from(next);
            }
        }
        all += w;
        if n > 0 {
            got += w * ok as f64 / n as f64;
        }
    }
    (all > 0.0).then(|| 100.0 * got / all)
}

/// Lit rooms passing the proxy: exterior glazing of at least an eighth of the floor, and no
/// point of the room deeper from its glazed walls than 2.5 window head heights.
fn daylight(p: &GProject) -> (Option<f64>, Option<f64>) {
    let (mut ok, mut all, mut wfr) = (0usize, 0usize, 0.0);
    for st in all_storeys(p) {
        let l = &st.layout;
        for (g, _) in rooms(st).filter(|(_, r)| r.kind.habitable()) {
            all += 1;
            let lit: Vec<(&GWall, &GOpening)> = l
                .openings
                .iter()
                .filter(|o| !o.door)
                .filter_map(|o| l.walls.get(o.wall).map(|w| (w, o)))
                .filter(|(w, o)| {
                    w.role == WallRole::Exterior && dist_poly(opening_centre(w, o), &g.polygon) < ON_ROOM
                })
                .collect();
            let a = area(&g.polygon).max(1e-9);
            let glass: f64 = lit.iter().map(|(_, o)| o.width * o.height).sum();
            let head = lit.iter().map(|(_, o)| o.sill + o.height).fold(0.0, f64::max);
            let depth = g
                .polygon
                .iter()
                .map(|v| lit.iter().map(|(w, _)| dist_seg(*v, w.a, w.b)).fold(f64::MAX, f64::min))
                .fold(0.0, f64::max);
            wfr += glass / a;
            let pass = !lit.is_empty()
                && glass >= GLAZING_SHARE * a - 1e-9
                && depth <= DEPTH_PER_HEAD * head + 1e-6;
            ok += usize::from(pass);
        }
    }
    (pct(ok, all), (all > 0).then(|| 100.0 * wfr / all as f64))
}

/// Longest way out over the door graph: from the farthest corner of a room to one of its doors,
/// then door to door through the rooms (straight inside each room) to an exit. Exits are the
/// exterior doors of the ground floor and the stair doors of every other storey.
fn egress(p: &GProject) -> (Option<f64>, Option<f64>) {
    let (mut worst, mut n, mut ok) = (None::<f64>, 0usize, 0usize);
    for (i, st) in all_storeys(p).enumerate() {
        let ground = i == 0;
        let l = &st.layout;
        let doors: Vec<(P, WallRole)> = l
            .openings
            .iter()
            .filter(|o| o.door)
            .filter_map(|o| l.walls.get(o.wall).map(|w| (opening_centre(w, o), w.role)))
            .collect();
        let list: Vec<(&GRoom, &PRoom)> = rooms(st).collect();
        let of_room: Vec<Vec<usize>> = list
            .iter()
            .map(|(g, _)| (0..doors.len()).filter(|&d| dist_poly(doors[d].0, &g.polygon) < ON_ROOM).collect())
            .collect();
        let mut dist = vec![f64::INFINITY; doors.len()];
        if ground {
            for (d, (_, role)) in doors.iter().enumerate() {
                if *role == WallRole::Exterior {
                    dist[d] = 0.0;
                }
            }
        } else {
            for (r, (_, m)) in list.iter().enumerate() {
                if m.kind == RoomKind::Stair {
                    for &d in &of_room[r] {
                        dist[d] = 0.0;
                    }
                }
            }
        }
        let mut done = vec![false; doors.len()];
        loop {
            let next = (0..doors.len())
                .filter(|&d| !done[d] && dist[d].is_finite())
                .min_by(|&a, &b| dist[a].total_cmp(&dist[b]));
            let Some(d) = next else { break };
            done[d] = true;
            for ds in of_room.iter().filter(|ds| ds.contains(&d)) {
                for &e in ds {
                    let via = dist[d] + hyp(doors[d].0, doors[e].0);
                    if via < dist[e] {
                        dist[e] = via;
                    }
                }
            }
        }
        for (r, (g, m)) in list.iter().enumerate() {
            if matches!(m.kind, RoomKind::Stair | RoomKind::Lift) {
                continue;
            }
            n += 1;
            let far = g
                .polygon
                .iter()
                .map(|v| {
                    of_room[r]
                        .iter()
                        .map(|&d| dist[d] + hyp(*v, doors[d].0))
                        .fold(f64::INFINITY, f64::min)
                })
                .fold(0.0, f64::max);
            if !of_room[r].is_empty() && far.is_finite() {
                ok += 1;
                worst = Some(worst.map_or(far, |w| w.max(far)));
            }
        }
    }
    (worst, pct(ok, n))
}

fn wet(st: &PStorey) -> Vec<P> {
    rooms(st)
        .filter(|(_, r)| matches!(r.kind, RoomKind::Bath | RoomKind::Wc))
        .map(|(g, _)| centroid(&g.polygon))
        .collect()
}

/// Baths and toilets above the ground floor with one right below (centres 1.5 m apart at most),
/// and all of them.
pub fn wet_stacked(p: &GProject) -> (usize, usize) {
    let (mut ok, mut all) = (0, 0);
    for k in 1..p.storeys.len() {
        let below = wet(&p.storeys[k - 1]);
        for c in wet(&p.storeys[k]) {
            all += 1;
            ok += usize::from(below.iter().any(|b| hyp(*b, c) <= STACK_REACH));
        }
    }
    (ok, all)
}

pub fn wet_stacking(p: &GProject) -> Option<f64> {
    let (ok, all) = wet_stacked(p);
    pct(ok, all)
}

/// Ground floor grid: per axis direction, one minus the spread (coefficient of variation) of the
/// spans, times the share of spans within the brief's largest span; directions averaged.
fn grid_regularity(s: &ProjectSpec, p: &GProject) -> Option<f64> {
    let l = &p.storeys.first()?.layout;
    if l.grids.is_empty() {
        return (!l.columns.is_empty()).then_some(0.0);
    }
    let mut groups: Vec<(f64, Vec<f64>)> = vec![];
    for g in &l.grids {
        let (dx, dy) = (g.b.0 - g.a.0, g.b.1 - g.a.1);
        if dx.hypot(dy) < 1e-9 {
            continue;
        }
        let mut ang = dy.atan2(dx).to_degrees().rem_euclid(180.0);
        if ang > 179.5 {
            ang -= 180.0;
        }
        let rad = ang.to_radians();
        let off = -g.a.0 * rad.sin() + g.a.1 * rad.cos();
        match groups.iter_mut().find(|x| (x.0 - ang).abs() < 0.5) {
            Some(x) => x.1.push(off),
            None => groups.push((ang, vec![off])),
        }
    }
    let mut scores = vec![];
    for (_, mut offs) in groups {
        offs.sort_by(f64::total_cmp);
        let spans: Vec<f64> = offs.windows(2).map(|w| w[1] - w[0]).filter(|x| *x > 0.01).collect();
        if spans.is_empty() {
            continue;
        }
        let n = spans.len() as f64;
        let mean = spans.iter().sum::<f64>() / n;
        let var = spans.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        let within = spans.iter().filter(|x| **x <= s.grid + 0.01).count() as f64 / n;
        scores.push((1.0 - var.sqrt() / mean).clamp(0.0, 1.0) * within);
    }
    if scores.is_empty() {
        return Some(0.0);
    }
    Some(100.0 * scores.iter().sum::<f64>() / scores.len() as f64)
}

/// Columns above the ground floor standing on a column of the storey below.
fn column_continuity(p: &GProject) -> Option<f64> {
    let (mut ok, mut all) = (0, 0);
    for k in 1..p.storeys.len() {
        for c in &p.storeys[k].layout.columns {
            all += 1;
            ok += usize::from(p.storeys[k - 1].layout.columns.iter().any(|u| hyp(u.at, c.at) < 0.05));
        }
    }
    pct(ok, all)
}

/// Plumbing risers the layout needs: wet rooms (bath, wc, kitchen) of all storeys grouped by
/// plan position; per 1000 m2 of gross area.
fn risers(p: &GProject, gross: f64) -> (Option<f64>, usize) {
    let mut stacks: Vec<P> = vec![];
    for st in &p.storeys {
        for (g, _) in rooms(st).filter(|(_, r)| matches!(r.kind, RoomKind::Bath | RoomKind::Wc | RoomKind::Kitchen)) {
            let c = centroid(&g.polygon);
            if !stacks.iter().any(|s| hyp(*s, c) <= STACK_REACH) {
                stacks.push(c);
            }
        }
    }
    ((gross > 0.0).then(|| 1000.0 * stacks.len() as f64 / gross), stacks.len())
}

fn overlap(a: &GRoom, b: &GRoom) -> bool {
    fn deep(p: P, poly: &[P]) -> bool {
        inside(p, poly) && dist_poly(p, poly) > ON_ROOM
    }
    // A point well inside the room: its label point, else the mean of its corners.
    fn probe(r: &GRoom) -> Option<P> {
        if deep(r.point, &r.polygon) {
            return Some(r.point);
        }
        let c = centroid(&r.polygon);
        deep(c, &r.polygon).then_some(c)
    }
    if probe(a).is_some_and(|q| deep(q, &b.polygon)) || probe(b).is_some_and(|q| deep(q, &a.polygon)) {
        return true;
    }
    let (na, nb) = (a.polygon.len(), b.polygon.len());
    (0..na).any(|i| {
        (0..nb).any(|j| crosses(a.polygon[i], a.polygon[(i + 1) % na], b.polygon[j], b.polygon[(j + 1) % nb]))
    })
}

/// Rooms overlapping, walls crossing or lying on each other, openings overlapping or running
/// past their wall.
fn clashes(p: &GProject) -> (usize, J) {
    let (mut room_n, mut wall_n, mut open_n) = (0usize, 0usize, 0usize);
    for st in all_storeys(p) {
        let l = &st.layout;
        let boxes: Vec<(P, P)> = l.rooms.iter().map(|r| bbox(&r.polygon)).collect();
        for i in 0..l.rooms.len() {
            for j in i + 1..l.rooms.len() {
                let (a, b) = (boxes[i], boxes[j]);
                if a.1 .0 <= b.0 .0 + 0.01 || b.1 .0 <= a.0 .0 + 0.01 || a.1 .1 <= b.0 .1 + 0.01 || b.1 .1 <= a.0 .1 + 0.01 {
                    continue;
                }
                room_n += usize::from(overlap(&l.rooms[i], &l.rooms[j]));
            }
        }
        for i in 0..l.walls.len() {
            for j in i + 1..l.walls.len() {
                let (a, b) = (&l.walls[i], &l.walls[j]);
                let double = seg_overlap(a.a, a.b, b.a, b.b) > 0.05;
                wall_n += usize::from(double || crosses(a.a, a.b, b.a, b.b));
            }
        }
        let mut by_wall: BTreeMap<usize, Vec<&GOpening>> = BTreeMap::new();
        for o in &l.openings {
            by_wall.entry(o.wall).or_default().push(o);
        }
        for (w, mut os) in by_wall {
            let Some(wall) = l.walls.get(w) else {
                open_n += os.len();
                continue;
            };
            let len = hyp(wall.a, wall.b);
            os.sort_by(|a, b| a.along.total_cmp(&b.along));
            for (i, o) in os.iter().enumerate() {
                if o.along < -0.01 || o.along + o.width > len + 0.01 {
                    open_n += 1;
                }
                if i > 0 {
                    let q = os[i - 1];
                    if o.along < q.along + q.width - 0.01 {
                        open_n += 1;
                    }
                }
            }
        }
    }
    (
        room_n + wall_n + open_n,
        json!({"rooms": room_n, "walls": wall_n, "openings": open_n}),
    )
}

/// Slab corners of every storey above ground on the plot.
fn site_fit(p: &GProject, plot: Option<&[P]>) -> Option<f64> {
    let plot = plot?;
    let pts: Vec<P> = p.storeys.iter().flat_map(|s| s.layout.slab.iter().copied()).collect();
    let ok = pts.iter().filter(|q| inside(**q, plot) || dist_poly(**q, plot) < ON_ROOM).count();
    pct(ok, pts.len())
}

/// Fall of the terrain across the ground slab along the downhill direction, m.
fn terrain_drop(p: &GProject, slope: Option<(f64, P)>) -> Option<f64> {
    let (s, d) = slope?;
    let n = d.0.hypot(d.1).max(1e-9);
    let proj: Vec<f64> = p.storeys.first()?.layout.slab.iter().map(|q| (q.0 * d.0 + q.1 * d.1) / n).collect();
    let lo = proj.iter().copied().fold(f64::MAX, f64::min);
    let hi = proj.iter().copied().fold(f64::MIN, f64::max);
    (hi >= lo).then(|| s * (hi - lo))
}

/// Elements whose decision a reason of `explain_project` names: walls by the massing or room
/// rules, rooms by their storey, doors by a door rule of their exact width, windows by the
/// glazing rule, columns and grids by the grid rule, stairs by the stair rule of their width,
/// shafts by the lift room rule, basement elements by the basement rules, roofs by the massing.
fn why_coverage(s: &ProjectSpec, p: &GProject) -> (Option<f64>, J) {
    let whys = explain_project(s, p);
    let has = |t: &str, rule: &str| whys.iter().any(|w| w.target == t && w.rule == rule);
    let is = |t: &str, rule: &str, x: f64| {
        whys.iter().any(|w| w.target == t && w.rule == rule && w.value.as_f64().is_some_and(|v| (v - x).abs() < 1e-6))
    };
    let door = |o: &GOpening| ["room.door", "room.door_wet", "room.door_wide"].iter().any(|r| is("rooms", *r, o.width));
    let mut tally: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
    let mut count = |class: &'static str, ok: bool| {
        let e = tally.entry(class).or_default();
        e.1 += 1;
        e.0 += usize::from(ok);
    };
    let floors = p.storeys.iter().enumerate().map(|(k, st)| (st, Some(k)));
    let below = p.basements.iter().map(|st| (st, None));
    for (st, k) in floors.chain(below) {
        let l = &st.layout;
        let own = match k {
            Some(k) => has(format!("storey:{k}").as_str(), "massing.floor_height"),
            None => has("basement", "basement.count"),
        };
        for w in &l.walls {
            let ok = match (k, w.role) {
                (None, _) => own,
                (_, WallRole::Exterior) => has("massing", "massing.form"),
                (_, WallRole::Corridor) => has("massing", "corridor.width"),
                _ => own && has("rooms", "room.min_unit"),
            };
            count("walls", ok);
        }
        for _ in &l.rooms {
            count("rooms", own);
        }
        for o in &l.openings {
            if o.door {
                count("doors", door(o));
            } else {
                count("windows", has("facade", "facade.glazing_share"));
            }
        }
        for _ in 0..l.columns.len() + l.grids.len() {
            count("grid", has("grid", "grid.span"));
        }
        for x in &l.stairs {
            count("stairs", is("core", "core.stair_width", x.width));
        }
        for _ in &l.shafts {
            count("shafts", has("core", "core.lift_room"));
        }
    }
    for _ in std::iter::once(&p.roof).chain(&p.extra_roofs) {
        count("roofs", has("massing", "massing.floor_height"));
    }
    let (ok, all) = tally.values().fold((0, 0), |a, v| (a.0 + v.0, a.1 + v.1));
    let missing: Map<String, J> = tally
        .iter()
        .filter(|(_, v)| v.0 < v.1)
        .map(|(k, v)| (k.to_string(), json!(v.1 - v.0)))
        .collect();
    (pct(ok, all), J::Object(missing))
}

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Builds the brief twice (the second build checks the result does not change) and measures it.
pub fn measure(b: &Brief) -> Row {
    let refuse = |why: String| Row {
        name: b.name.clone(),
        group: b.group.clone(),
        refused: Some(why),
        metrics: METRICS.iter().map(|m| (m.id, None)).collect(),
        detail: J::Null,
    };
    let s = match parse_spec(&spec_of(b)) {
        Ok(s) => s,
        Err(e) => return refuse(format!("{e}")),
    };
    let t = Instant::now();
    let p = match build_project(&s) {
        Ok(p) => p,
        Err(e) => return refuse(format!("{e}")),
    };
    let first = t.elapsed();
    let t = Instant::now();
    let again = build_project(&s);
    let second = t.elapsed();
    let same = again.as_ref().is_ok_and(|q| format!("{q:?}") == format!("{p:?}"));
    let ms = first.min(second).as_secs_f64() * 1000.0;

    let gfa = gross(&p);
    let (prog, missing_rooms) = programme_error(&s, &p);
    let (day, wfr) = daylight(&p);
    let (egress_max, egress_ok) = egress(&p);
    let (riser_rate, riser_n) = risers(&p, gfa);
    let gates = check_project(&s, &p);
    let (clash_n, clash_detail) = clashes(&p);
    let (why, why_missing) = why_coverage(&s, &p);
    let wished = wishes(&s, b);
    let values: [(&'static str, Option<f64>); 18] = [
        ("area_efficiency", area_efficiency(&p, gfa)),
        ("programme_error", prog),
        ("adjacency", adjacency(&p, &wished)),
        ("circulation_ratio", circulation_ratio(&p)),
        ("daylight", day),
        ("egress_max", egress_max),
        ("egress_reachable", egress_ok),
        ("wet_stacking", wet_stacking(&p)),
        ("grid_regularity", grid_regularity(&s, &p)),
        ("column_continuity", column_continuity(&p)),
        ("risers_per_1000m2", riser_rate),
        ("norm_findings", Some((gates.failures.len() + gates.warnings.len()) as f64)),
        ("hard_failures", Some(gates.failures.len() as f64)),
        ("clashes", Some(clash_n as f64)),
        ("site_fit", site_fit(&p, b.plot.as_deref())),
        ("deterministic", Some(if same { 100.0 } else { 0.0 })),
        ("why_coverage", why),
        (RUNTIME, Some(ms)),
    ];
    let metrics: BTreeMap<&'static str, Option<f64>> = values.into_iter().map(|(k, v)| (k, v.map(r2))).collect();
    debug_assert!(METRICS.iter().all(|m| metrics.contains_key(m.id)));
    let detail = json!({
        "storeys": p.storeys.len(),
        "basements": p.basements.len(),
        "gross_m2": r2(gfa),
        "rooms": all_storeys(&p).map(|s| s.rooms.len()).sum::<usize>(),
        "missing_rooms": missing_rooms,
        "mean_window_to_floor_pct": wfr.map(r2),
        "risers": riser_n,
        "clashes": clash_detail,
        "failures": gates.failures.iter().take(5).collect::<Vec<_>>(),
        "warnings": gates.warnings.len(),
        "unexplained": why_missing,
        "terrain_drop_m": terrain_drop(&p, b.slope).map(r2),
        "ground_z_m": p.site.terrain.then(|| r2(p.site.ground_z)),
        "cut_m3": p.site.terrain.then(|| r2(p.site.cut)),
        "fill_m3": p.site.terrain.then(|| r2(p.site.fill)),
        "retaining_walls": p.site.walls.len(),
        "stepped_plinth": p.site.stepped,
        "split_drop_m": p.site.split.as_ref().map(|sp| r2(sp.drop)),
    });
    Row {
        name: b.name.clone(),
        group: b.group.clone(),
        refused: None,
        metrics,
        detail,
    }
}

pub fn run_all(briefs: &[Brief]) -> Vec<Row> {
    briefs.iter().map(measure).collect()
}

pub fn total_runtime(rows: &[Row]) -> f64 {
    r2(rows.iter().filter_map(|r| r.get(RUNTIME)).sum())
}

/// Mean of each metric over the briefs it applies to.
pub fn means(rows: &[Row]) -> BTreeMap<&'static str, Option<f64>> {
    METRICS
        .iter()
        .map(|m| {
            let xs: Vec<f64> = rows.iter().filter_map(|r| r.get(m.id)).collect();
            (m.id, (!xs.is_empty()).then(|| r2(xs.iter().sum::<f64>() / xs.len() as f64)))
        })
        .collect()
}

/// The scoreboard: what `bench/baseline.json` pins.
pub fn report(rows: &[Row]) -> J {
    let mut briefs = Map::new();
    for r in rows {
        briefs.insert(
            r.name.clone(),
            json!({"group": r.group, "refused": r.refused, "metrics": r.metrics, "detail": r.detail}),
        );
    }
    json!({
        "schema": "gen_bench_baseline.v1",
        "briefs": briefs,
        "mean": means(rows),
        "runtime_ms_total": total_runtime(rows),
    })
}

/// What got worse than the baseline beyond its tolerance; a brief or metric the baseline does
/// not pin is a finding too, so the scoreboard always covers the whole bench.
pub fn regressions(base: &J, rows: &[Row]) -> Vec<String> {
    let mut out = vec![];
    let Some(pinned) = base["briefs"].as_object() else {
        return vec!["the baseline pins no briefs".to_string()];
    };
    for r in rows.iter().filter(|r| !pinned.contains_key(&r.name)) {
        out.push(format!("{}: not pinned in the baseline", r.name));
    }
    for (name, was) in pinned {
        let Some(r) = rows.iter().find(|r| &r.name == name) else {
            out.push(format!("{name}: pinned but missing from data/bench"));
            continue;
        };
        let was_built = was["refused"].is_null();
        if let (true, Some(why)) = (was_built, &r.refused) {
            out.push(format!("{name}: no longer builds: {why}"));
            continue;
        }
        for m in METRICS.iter().filter(|m| m.id != RUNTIME) {
            let Some(old) = was["metrics"].get(m.id) else {
                if was_built {
                    out.push(format!("{name}: {} not pinned in the baseline", m.id));
                }
                continue;
            };
            let Some(old) = old.as_f64() else { continue };
            match r.get(m.id) {
                None => out.push(format!("{name}: {} no longer measured (was {old})", m.id)),
                Some(now) => {
                    let worse = match m.better {
                        Better::Higher => old - now,
                        Better::Lower => now - old,
                    };
                    if worse > m.tol + 1e-9 {
                        out.push(format!("{name}: {} {old} -> {now} (tolerance {})", m.id, m.tol));
                    }
                }
            }
        }
    }
    if let Some(old) = base["runtime_ms_total"].as_f64() {
        let now = total_runtime(rows);
        if now > RUNTIME_FACTOR * old + RUNTIME_SLACK_MS {
            out.push(format!("{RUNTIME} total {old} -> {now} (allowed x{RUNTIME_FACTOR} + {RUNTIME_SLACK_MS})"));
        }
    }
    out
}

fn cell(x: Option<f64>) -> String {
    match x {
        Some(v) => format!("{v:>8.1}"),
        None => format!("{:>8}", "-"),
    }
}

/// The scoreboard as a text table: a row per brief, the means, then the legend.
pub fn table(rows: &[Row]) -> String {
    let mut s = format!("{:<26}", "brief");
    for m in &METRICS {
        s += &format!("{:>8}", m.short);
    }
    s.push('\n');
    for r in rows {
        s += &format!("{:<26}", r.name);
        match &r.refused {
            Some(why) => s += &format!("  refused: {why}"),
            None => {
                for m in &METRICS {
                    s += &cell(r.get(m.id));
                }
            }
        }
        s.push('\n');
    }
    let mean = means(rows);
    s += &format!("{:<26}", "MEAN");
    for m in &METRICS {
        s += &cell(mean.get(m.id).copied().flatten());
    }
    s += &format!("\n\n{} briefs, {} metrics, total build {:.0} ms\n", rows.len(), METRICS.len(), total_runtime(rows));
    for m in &METRICS {
        let dir = match m.better {
            Better::Higher => "higher",
            Better::Lower => "lower",
        };
        s += &format!("  {:<7} {:<20} {:<24} {dir} is better, tolerance {}\n", m.short, m.id, m.unit, m.tol);
    }
    s
}

#[cfg(test)]
mod geometry_tests {
    use super::*;

    #[test]
    fn neighbours_share_a_wall_and_corners_do_not() {
        let a = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)];
        let b = [(4.0, 1.0), (8.0, 1.0), (8.0, 5.0), (4.0, 5.0)];
        let c = [(4.0, 3.0), (6.0, 3.0), (6.0, 6.0), (4.0, 6.0)];
        assert!((shared_edge(&a, &b) - 2.0).abs() < 1e-9);
        assert_eq!(shared_edge(&a, &c), 0.0);
    }

    #[test]
    fn a_cross_clashes_and_a_tee_does_not() {
        assert!(crosses((0.0, 0.0), (4.0, 0.0), (2.0, -1.0), (2.0, 1.0)));
        assert!(!crosses((0.0, 0.0), (4.0, 0.0), (2.0, 0.0), (2.0, 1.0)));
        assert!(!crosses((0.0, 0.0), (4.0, 0.0), (5.0, -1.0), (5.0, 1.0)));
        assert!(seg_overlap((0.0, 0.0), (4.0, 0.0), (3.0, 0.0), (6.0, 0.0)) > 0.99);
    }

    #[test]
    fn overlapping_rooms_clash() {
        let room = |poly: Vec<P>| GRoom { name: String::new(), apartment: None, point: centroid(&poly), polygon: poly };
        let a = room(vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        let b = room(vec![(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0)]);
        let c = room(vec![(4.0, 0.0), (8.0, 0.0), (8.0, 4.0), (4.0, 4.0)]);
        assert!(overlap(&a, &b));
        assert!(overlap(&a, &a.clone()));
        assert!(!overlap(&a, &c));
        assert!(inside((1.0, 1.0), &a.polygon) && !inside((5.0, 1.0), &a.polygon));
    }
}
