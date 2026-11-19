// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Realism critic (docs/design/AI_GEOMETRY_PLAN.md section 3.4): code, not a prompt. A bent
//! straight profile, axes that miss a node, slivers, thick plates, bolt distances, rebar cover,
//! stairs, doors and windows, parts hanging in the air. The rules run on `Input`, plain data in
//! metres, so every rule has a pass and a fail fixture without a database; `realism_input.rs`
//! fills it from the model. Findings go to the AI as tasks (`ai.realism.*`).

use std::collections::HashMap;
use std::sync::Arc;

use ddc_geom::mesh::Mesh;
use ddc_geom::mesh_dist;

use super::util::{Sink, Within};
use super::{CheckRule, CheckScope, Finding, Fix, Measure, Severity};
use crate::{ElKey, Error, Model};

/// Axes of members meeting at a node agree within this, m.
pub const AXIS_TOL: f64 = 0.001;
/// Parts thinner than this in any direction are not buildable, m.
pub const MIN_PART: f64 = 0.005;
/// Gaps between parts: 0 < g < SLIVER_MAX is a sliver; below `TOUCH` the parts touch, m.
pub const SLIVER_MAX: f64 = 0.001;
pub const TOUCH: f64 = 1e-6;
/// A plate may be this many times thicker than the thinnest element it joins.
pub const PLATE_RATIO_MAX: f64 = 4.0;
/// Bolt diameters of the catalog series, mm.
pub const BOLT_SERIES_MM: [f64; 22] = [
    6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 27.0, 30.0, 33.0, 36.0, 39.0,
    42.0, 45.0, 48.0, 52.0, 56.0, 60.0, 64.0,
];
/// Hole minus bolt diameter, mm: from close fit to the largest normal clearance.
pub const HOLE_CLEARANCE_MM: (f64, f64) = (0.5, 3.0);
/// Minimum end/edge distance and pitch, in hole diameters.
pub const EDGE_MIN: f64 = 1.2;
pub const PITCH_MIN: f64 = 2.2;
/// Stirrup spacing limits, m.
pub const STIRRUP_SPACING: (f64, f64) = (0.05, 0.30);
/// Cover floor whatever the host asks, m.
pub const COVER_FLOOR: f64 = 0.015;
/// Stair limits, m (planning defaults, not a code claim).
pub const RISER: (f64, f64) = (0.10, 0.20);
pub const TREAD: (f64, f64) = (0.22, 0.40);
/// Parts may rest on the lowest level within this, m.
pub const GROUND_TOL: f64 = 0.005;
const MAX_PAIRS: usize = 3_000_000;
const EXACT_GAP_PAIRS: usize = 60_000;

/// Section class for the bend table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// I, H, C, T, Z, L.
    Open,
    /// Tubes and rings.
    Hollow,
    /// Solid rect or round.
    Bar,
}

impl Shape {
    pub fn name(self) -> &'static str {
        match self {
            Shape::Open => "open",
            Shape::Hollow => "hollow",
            Shape::Bar => "bar",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BendMethod {
    Cold,
    Hot,
}

impl BendMethod {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cold" => Some(BendMethod::Cold),
            "hot" => Some(BendMethod::Hot),
            _ => None,
        }
    }
}

/// Smallest centreline radius a section may be bent to, m. Planning values: the radius as a
/// multiple of the section depth, by class and method.
pub fn min_bend_radius(shape: Shape, depth: f64, method: BendMethod) -> f64 {
    let k = match (shape, method) {
        (Shape::Open, BendMethod::Cold) => 25.0,
        (Shape::Open, BendMethod::Hot) => 10.0,
        (Shape::Hollow, BendMethod::Cold) => 3.5,
        (Shape::Hollow, BendMethod::Hot) => 2.0,
        (Shape::Bar, BendMethod::Cold) => 2.0,
        (Shape::Bar, BendMethod::Hot) => 1.0,
    };
    k * depth
}

/// A linear member: a rolled or built-up section along an axis.
#[derive(Clone, Debug)]
pub struct Member {
    pub key: ElKey,
    pub shape: Shape,
    /// Largest section dimension.
    pub depth: f64,
    /// Axis points; arcs are sampled finely enough for 0.1 mm of sag.
    pub axis: Vec<[f64; 3]>,
    pub closed: bool,
    /// Smallest radius of curvature of the axis; `Some(0.0)` is a sharp kink; `None` straight.
    pub radius: Option<f64>,
    /// Typed as bent, with the method.
    pub bend: Option<BendMethod>,
    /// The eccentricity at the ends is declared on purpose.
    pub ecc: bool,
}

/// Any solid body.
#[derive(Clone, Debug)]
pub struct Part {
    pub key: ElKey,
    pub bbox: [f64; 6],
    /// Thinnest extent: section side or length, whichever is smaller.
    pub min_dim: f64,
    pub mesh: Option<Arc<Mesh>>,
}

#[derive(Clone, Debug)]
pub struct Plate {
    pub key: ElKey,
    pub thick: f64,
    /// Elements it joins with their thickness.
    pub joined: Vec<(ElKey, f64)>,
}

#[derive(Clone, Debug)]
pub struct Bolt {
    pub key: ElKey,
    pub d: f64,
    pub hole: Option<f64>,
    /// Distance from the bolt axis to the nearest plate edge.
    pub edge: Option<f64>,
    /// Distance to the nearest other bolt in the same plate.
    pub pitch: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Rebar {
    pub key: ElKey,
    pub d: f64,
    /// Smallest configured cover over the faces of the host.
    pub cover: Option<f64>,
    /// Smallest and largest stirrup spacing the layout allows.
    pub spacing: Option<(f64, f64)>,
}

#[derive(Clone, Debug)]
pub struct Stair {
    pub key: ElKey,
    pub riser: f64,
    pub tread: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Input {
    pub parts: Vec<Part>,
    pub members: Vec<Member>,
    pub plates: Vec<Plate>,
    pub bolts: Vec<Bolt>,
    pub rebar: Vec<Rebar>,
    pub stairs: Vec<Stair>,
    /// Bounds of walls, slabs, columns and footings that carry parts.
    pub supports: Vec<[f64; 6]>,
    /// Top of the lowest level: parts resting here are grounded. `None` skips "floating".
    pub ground_z: Option<f64>,
}

/// One finding of the pure rules.
#[derive(Clone, Debug)]
pub struct Issue {
    pub severity: Severity,
    pub keys: Vec<ElKey>,
    pub at: Option<[f64; 3]>,
    pub msg_key: &'static str,
    pub args: Vec<(&'static str, String)>,
    pub measure: Option<Measure>,
}

pub const K_BENT: &str = "ai.realism.bent_straight";
pub const K_RADIUS: &str = "ai.realism.bend_radius";
pub const K_AXIS: &str = "ai.realism.axis_miss";
pub const K_TINY: &str = "ai.realism.tiny_part";
pub const K_SLIVER: &str = "ai.realism.sliver_gap";
pub const K_PLATE: &str = "ai.realism.plate_thick";
pub const K_BOLT_D: &str = "ai.realism.bolt_dia";
pub const K_BOLT_HOLE: &str = "ai.realism.bolt_hole";
pub const K_BOLT_EDGE: &str = "ai.realism.bolt_edge";
pub const K_BOLT_PITCH: &str = "ai.realism.bolt_pitch";
pub const K_COVER: &str = "ai.realism.cover";
pub const K_STIRRUP: &str = "ai.realism.stirrup";
pub const K_RISER: &str = "ai.realism.stair_riser";
pub const K_TREAD: &str = "ai.realism.stair_tread";
pub const K_FLOAT: &str = "ai.realism.floating";

/// Every message key of the pure rules.
pub const KEYS: [&str; 15] = [
    K_BENT, K_RADIUS, K_AXIS, K_TINY, K_SLIVER, K_PLATE, K_BOLT_D, K_BOLT_HOLE, K_BOLT_EDGE,
    K_BOLT_PITCH, K_COVER, K_STIRRUP, K_RISER, K_TREAD, K_FLOAT,
];

fn name(k: &ElKey) -> String {
    format!("{}:{}", k.0, k.1)
}

fn mm(v: f64) -> f64 {
    v * 1000.0
}

fn issue(
    severity: Severity,
    keys: Vec<ElKey>,
    at: Option<[f64; 3]>,
    msg_key: &'static str,
    args: Vec<(&'static str, String)>,
    measure: Option<Measure>,
) -> Issue {
    Issue { severity, keys, at, msg_key, args, measure }
}

fn txt(v: f64) -> String {
    format!("{:.1}", mm(v))
}

fn mid(a: &[[f64; 3]]) -> Option<[f64; 3]> {
    a.get(a.len() / 2).copied()
}

// ---------- geometry helpers ----------

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn dist_seg(p: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
    let ab = sub(b, a);
    let l2 = dot(ab, ab);
    let t = if l2 < 1e-24 { 0.0 } else { (dot(sub(p, a), ab) / l2).clamp(0.0, 1.0) };
    let q = [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t];
    dot(sub(p, q), sub(p, q)).sqrt()
}

fn dist_polyline(p: [f64; 3], pts: &[[f64; 3]]) -> f64 {
    match pts {
        [] => f64::MAX,
        [one] => dot(sub(p, *one), sub(p, *one)).sqrt(),
        _ => pts.windows(2).map(|w| dist_seg(p, w[0], w[1])).fold(f64::MAX, f64::min),
    }
}

fn box_gap(a: &[f64; 6], b: &[f64; 6]) -> f64 {
    let mut s = 0.0;
    for k in 0..3 {
        let d = (a[k] - b[k + 3]).max(b[k] - a[k + 3]).max(0.0);
        s += d * d;
    }
    s.sqrt()
}

/// Index pairs of boxes whose gap is at most `tol`, by a sweep along x.
pub(super) fn near_pairs(boxes: &[[f64; 6]], tol: f64) -> Vec<(usize, usize)> {
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|&a, &b| boxes[a][0].total_cmp(&boxes[b][0]));
    let mut out = vec![];
    let mut active: Vec<usize> = vec![];
    for &i in &order {
        active.retain(|&j| boxes[j][3] + tol >= boxes[i][0]);
        for &j in &active {
            if box_gap(&boxes[i], &boxes[j]) <= tol {
                out.push((i.min(j), i.max(j)));
                if out.len() >= MAX_PAIRS {
                    return out;
                }
            }
        }
        active.push(i);
    }
    out
}

fn center(b: &[f64; 6]) -> [f64; 3] {
    [(b[0] + b[3]) / 2.0, (b[1] + b[4]) / 2.0, (b[2] + b[5]) / 2.0]
}

// ---------- rules ----------

fn rule_bent(inp: &Input, out: &mut Vec<Issue>) {
    for m in &inp.members {
        let Some(r) = m.radius else { continue };
        let at = mid(&m.axis);
        match m.bend {
            None => out.push(issue(
                Severity::Error,
                vec![m.key.clone()],
                at,
                K_BENT,
                vec![("name", name(&m.key)), ("shape", m.shape.name().to_string())],
                Some(Measure::flag()),
            )),
            Some(method) => {
                let need = min_bend_radius(m.shape, m.depth, method);
                if r + 1e-9 < need {
                    out.push(issue(
                        Severity::Error,
                        vec![m.key.clone()],
                        at,
                        K_RADIUS,
                        vec![
                            ("name", name(&m.key)),
                            ("radius", txt(r)),
                            ("min", txt(need)),
                        ],
                        Some(Measure::new(mm(r), mm(need), "mm")),
                    ));
                }
            }
        }
    }
}

/// Members indexed by their bounds grown by the reach of a node, 1 m cells.
struct Grid {
    cells: HashMap<(i64, i64, i64), Vec<usize>>,
    wide: Vec<usize>,
}

fn cell(v: f64) -> i64 {
    v.floor() as i64
}

fn reach(m: &Member) -> f64 {
    0.5 * m.depth + 0.05
}

impl Grid {
    fn new(members: &[Member]) -> Grid {
        let mut g = Grid { cells: HashMap::new(), wide: vec![] };
        for (i, m) in members.iter().enumerate() {
            let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
            for p in &m.axis {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
            if lo[0] > hi[0] {
                continue;
            }
            let r = reach(m);
            let a = (cell(lo[0] - r), cell(lo[1] - r), cell(lo[2] - r));
            let b = (cell(hi[0] + r), cell(hi[1] + r), cell(hi[2] + r));
            let n = (b.0 - a.0 + 1) * (b.1 - a.1 + 1) * (b.2 - a.2 + 1);
            if n > 4096 {
                g.wide.push(i);
                continue;
            }
            for x in a.0..=b.0 {
                for y in a.1..=b.1 {
                    for z in a.2..=b.2 {
                        g.cells.entry((x, y, z)).or_default().push(i);
                    }
                }
            }
        }
        g
    }

    fn near(&self, p: [f64; 3]) -> impl Iterator<Item = usize> + '_ {
        let c = (cell(p[0]), cell(p[1]), cell(p[2]));
        self.cells.get(&c).into_iter().flatten().chain(self.wide.iter()).copied()
    }
}

fn rule_axis(inp: &Input, out: &mut Vec<Issue>) {
    let grid = Grid::new(&inp.members);
    let mut seen = std::collections::HashSet::new();
    for (i, m) in inp.members.iter().enumerate() {
        if m.closed || m.ecc || m.axis.len() < 2 {
            continue;
        }
        for end in [m.axis[0], m.axis[m.axis.len() - 1]] {
            let mut touching = false;
            let mut nearest: Option<(usize, f64)> = None;
            for j in grid.near(end) {
                if j == i || inp.members[j].ecc {
                    continue;
                }
                let o = &inp.members[j];
                let d = dist_polyline(end, &o.axis);
                if d <= AXIS_TOL {
                    touching = true;
                    break;
                }
                if d <= reach(o) && nearest.is_none_or(|n| d < n.1) {
                    nearest = Some((j, d));
                }
            }
            if let (false, Some((j, d))) = (touching, nearest) {
                // One finding per pair of members, whichever end sees the miss first.
                if !seen.insert((i.min(j), i.max(j))) {
                    continue;
                }
                let o = &inp.members[j];
                out.push(issue(
                    Severity::Error,
                    vec![m.key.clone(), o.key.clone()],
                    Some(end),
                    K_AXIS,
                    vec![("a", name(&m.key)), ("b", name(&o.key)), ("gap", txt(d))],
                    Some(Measure::new(mm(d), mm(AXIS_TOL), "mm")),
                ));
            }
        }
    }
}

fn rule_tiny(inp: &Input, out: &mut Vec<Issue>) {
    for p in &inp.parts {
        if p.min_dim < MIN_PART {
            out.push(issue(
                Severity::Error,
                vec![p.key.clone()],
                Some(center(&p.bbox)),
                K_TINY,
                vec![("name", name(&p.key)), ("size", txt(p.min_dim))],
                Some(Measure::new(mm(p.min_dim), mm(MIN_PART), "mm")),
            ));
        }
    }
}

fn rule_sliver(inp: &Input, out: &mut Vec<Issue>) {
    let boxes: Vec<[f64; 6]> = inp.parts.iter().map(|p| p.bbox).collect();
    let mut exact = 0usize;
    for (i, j) in near_pairs(&boxes, SLIVER_MAX) {
        let (a, b) = (&inp.parts[i], &inp.parts[j]);
        let gap = match (&a.mesh, &b.mesh) {
            (Some(x), Some(y)) if exact < EXACT_GAP_PAIRS => {
                exact += 1;
                mesh_dist::mesh_distance(x, y).map(|c| c.dist)
            }
            _ => Some(box_gap(&a.bbox, &b.bbox)),
        };
        let Some(g) = gap else { continue };
        if g > TOUCH && g < SLIVER_MAX {
            out.push(issue(
                Severity::Warning,
                vec![a.key.clone(), b.key.clone()],
                Some(center(&a.bbox)),
                K_SLIVER,
                vec![("a", name(&a.key)), ("b", name(&b.key)), ("gap", format!("{:.3}", mm(g)))],
                Some(Measure::new(mm(g), mm(SLIVER_MAX), "mm")),
            ));
        }
    }
}

fn rule_plate(inp: &Input, out: &mut Vec<Issue>) {
    for p in &inp.plates {
        let Some((thin, t)) = p
            .joined
            .iter()
            .filter(|j| j.1 > 0.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|j| (&j.0, j.1))
        else {
            continue;
        };
        if p.thick > PLATE_RATIO_MAX * t + 1e-9 {
            out.push(issue(
                Severity::Warning,
                vec![p.key.clone(), thin.clone()],
                None,
                K_PLATE,
                vec![("name", name(&p.key)), ("thick", txt(p.thick)), ("thin", txt(t))],
                Some(Measure::new(p.thick / t, PLATE_RATIO_MAX, "count")),
            ));
        }
    }
}

fn rule_bolt(inp: &Input, out: &mut Vec<Issue>) {
    for b in &inp.bolts {
        let dm = mm(b.d);
        let k = vec![b.key.clone()];
        if !BOLT_SERIES_MM.iter().any(|s| (s - dm).abs() < 0.05) {
            out.push(issue(
                Severity::Error,
                k.clone(),
                None,
                K_BOLT_D,
                vec![("name", name(&b.key)), ("d", txt(b.d))],
                Some(Measure::flag()),
            ));
        }
        let d0 = b.hole.unwrap_or(b.d + 0.002);
        if let Some(h) = b.hole {
            let c = mm(h - b.d);
            if c < HOLE_CLEARANCE_MM.0 - 1e-9 || c > HOLE_CLEARANCE_MM.1 + 1e-9 {
                out.push(issue(
                    Severity::Error,
                    k.clone(),
                    None,
                    K_BOLT_HOLE,
                    vec![("name", name(&b.key)), ("hole", txt(h)), ("d", txt(b.d))],
                    Some(Measure::new(c, HOLE_CLEARANCE_MM.1, "mm")),
                ));
            }
        }
        for (v, f, key) in [(b.edge, EDGE_MIN, K_BOLT_EDGE), (b.pitch, PITCH_MIN, K_BOLT_PITCH)] {
            if let Some(v) = v.filter(|v| *v + 1e-9 < f * d0) {
                out.push(issue(
                    Severity::Error,
                    k.clone(),
                    None,
                    key,
                    vec![("name", name(&b.key)), ("dist", txt(v)), ("min", txt(f * d0))],
                    Some(Measure::new(mm(v), mm(f * d0), "mm")),
                ));
            }
        }
    }
}

fn rule_rebar(inp: &Input, out: &mut Vec<Issue>) {
    for r in &inp.rebar {
        if let Some(c) = r.cover {
            let need = r.d.max(COVER_FLOOR);
            if c + 1e-9 < need {
                out.push(issue(
                    Severity::Error,
                    vec![r.key.clone()],
                    None,
                    K_COVER,
                    vec![("name", name(&r.key)), ("cover", txt(c)), ("min", txt(need))],
                    Some(Measure::new(mm(c), mm(need), "mm")),
                ));
            }
        }
        if let Some((lo, hi)) = r.spacing {
            if lo + 1e-9 < STIRRUP_SPACING.0 || hi > STIRRUP_SPACING.1 + 1e-9 {
                let bad = if hi > STIRRUP_SPACING.1 { hi } else { lo };
                out.push(issue(
                    Severity::Error,
                    vec![r.key.clone()],
                    None,
                    K_STIRRUP,
                    vec![
                        ("name", name(&r.key)),
                        ("spacing", txt(bad)),
                        ("min", txt(STIRRUP_SPACING.0)),
                        ("max", txt(STIRRUP_SPACING.1)),
                    ],
                    Some(Measure::new(mm(bad), mm(STIRRUP_SPACING.1), "mm")),
                ));
            }
        }
    }
}

fn rule_stair(inp: &Input, out: &mut Vec<Issue>) {
    for s in &inp.stairs {
        for (v, lim, key) in [(s.riser, RISER, K_RISER), (s.tread, TREAD, K_TREAD)] {
            if v + 1e-9 < lim.0 || v > lim.1 + 1e-9 {
                let limit = if v > lim.1 { lim.1 } else { lim.0 };
                out.push(issue(
                    Severity::Error,
                    vec![s.key.clone()],
                    None,
                    key,
                    vec![
                        ("name", name(&s.key)),
                        ("value", txt(v)),
                        ("min", txt(lim.0)),
                        ("max", txt(lim.1)),
                    ],
                    Some(Measure::new(mm(v), mm(limit), "mm")),
                ));
            }
        }
    }
}

fn find(uf: &mut [usize], mut i: usize) -> usize {
    while uf[i] != i {
        uf[i] = uf[uf[i]];
        i = uf[i];
    }
    i
}

fn rule_floating(inp: &Input, out: &mut Vec<Issue>) {
    let Some(ground) = inp.ground_z else { return };
    let n = inp.parts.len();
    let boxes: Vec<[f64; 6]> = inp.parts.iter().map(|p| p.bbox).collect();
    let mut uf: Vec<usize> = (0..n).collect();
    for (i, j) in near_pairs(&boxes, AXIS_TOL) {
        let (a, b) = (find(&mut uf, i), find(&mut uf, j));
        uf[a] = b;
    }
    let mut grounded = vec![false; n];
    for (i, b) in boxes.iter().enumerate() {
        let on_support = inp.supports.iter().any(|s| box_gap(b, s) <= AXIS_TOL);
        if b[2] <= ground + GROUND_TOL || on_support {
            let r = find(&mut uf, i);
            grounded[r] = true;
        }
    }
    let mut comps: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let r = find(&mut uf, i);
        comps.entry(r).or_default().push(i);
    }
    let mut floating: Vec<Vec<usize>> =
        comps.into_iter().filter(|(r, _)| !grounded[*r]).map(|(_, v)| v).collect();
    floating.sort_by_key(|v| v[0]);
    for c in floating {
        let low = c.iter().copied().min_by(|&a, &b| boxes[a][2].total_cmp(&boxes[b][2]));
        let Some(low) = low else { continue };
        let gap = boxes[low][2] - ground;
        out.push(issue(
            Severity::Error,
            c.iter().take(5).map(|&i| inp.parts[i].key.clone()).collect(),
            Some(center(&boxes[low])),
            K_FLOAT,
            vec![("name", name(&inp.parts[low].key)), ("count", c.len().to_string())],
            Some(Measure::new(mm(gap), 0.0, "mm")),
        ));
    }
}

/// All pure rules, in the order the person reads them.
pub fn analyze(inp: &Input) -> Vec<Issue> {
    let mut v = vec![];
    rule_bent(inp, &mut v);
    rule_axis(inp, &mut v);
    rule_tiny(inp, &mut v);
    rule_sliver(inp, &mut v);
    rule_plate(inp, &mut v);
    rule_bolt(inp, &mut v);
    rule_rebar(inp, &mut v);
    rule_stair(inp, &mut v);
    rule_floating(inp, &mut v);
    v
}

fn into_findings(
    issues: Vec<Issue>,
    w: &Within,
    id: &'static str,
    out: &mut Vec<Finding>,
) {
    let mut sink = Sink::new(out, id);
    for i in issues {
        if !i.keys.iter().any(|k| w.has(&k.0, k.1)) {
            continue;
        }
        let Some(m) = i.measure else { continue };
        sink.add(i.severity, i.keys, i.at, i.msg_key, i.args, Fix::None, m);
        if sink.full() {
            break;
        }
    }
    sink.done();
}

/// The registered rule: the pure checks over the solids, rebar and stairs of the model.
pub(super) struct Realism;

impl CheckRule for Realism {
    fn id(&self) -> &'static str {
        "realism"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let inp = super::realism_input::collect(m)?;
        into_findings(analyze(&inp), &Within::new(scope), self.id(), out);
        Ok(())
    }
}

/// What the AI sees after a step: the realism findings plus doors and windows that are not
/// hosted or intersect (the opening rules), all as plain findings.
pub fn critique(m: &Model, scope: &CheckScope) -> Result<Vec<Finding>, Error> {
    let mut out = vec![];
    Realism.run(m, scope, &mut out)?;
    for r in [
        &super::rule_opening::NoHost as &dyn CheckRule,
        &super::rule_opening::OutsideWall,
        &super::rule_opening::OpeningOverlap,
    ] {
        r.run(m, scope, &mut out)?;
    }
    super::finalize(m, &mut out);
    out.sort_by_key(|f| f.severity);
    Ok(out)
}
