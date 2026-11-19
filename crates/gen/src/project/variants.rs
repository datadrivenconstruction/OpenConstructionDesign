// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Design variants that differ on purpose. The free rules of a brief (massing form, grid span,
//! band depth, facade system: those the brief and the locks leave open) span a candidate pool;
//! each candidate is built, gated and scored, and the best ones are picked greedily under a
//! minimum pairwise distance, so the user is not offered near twins. The unchanged brief is
//! always a candidate, so the best variant is never worse than a single run. Seed-deterministic.

use super::locks::{Lock, Target};
use super::rules;
use super::score::{score, Score};
use super::{build_project, check_project, parse_spec, FacadeSystem, GProject, Massing, ProjectSpec};
use serde_json::{json, Map, Value as J};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// How many variants, from how large a pool, how far apart.
#[derive(Clone, Debug)]
pub struct VaryCfg {
    pub n: usize,
    /// Candidates built, the unchanged brief included.
    pub pool: usize,
    pub seed: u64,
    /// Smallest distance (0..=1) between two chosen variants; relaxed in halves when the pool
    /// has too few far-apart candidates.
    pub min_distance: f64,
    /// Rules that must keep their value (locked by the user).
    pub fixed: Vec<String>,
    /// Pinned parts every variant keeps; a rule lock also fixes its rule.
    pub locks: Vec<Lock>,
}

impl Default for VaryCfg {
    fn default() -> Self {
        VaryCfg {
            n: 3,
            pool: 12,
            seed: 0,
            min_distance: 0.25,
            fixed: vec![],
            locks: vec![],
        }
    }
}

/// One rule the variants may change, with the values tried.
#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    pub rule: &'static str,
    pub values: Vec<J>,
    /// The value of the unchanged brief.
    pub base: J,
}

/// A built candidate.
#[derive(Clone, Debug)]
pub struct Variant {
    /// The brief it was built from.
    pub brief: J,
    /// Every free axis and its value here, the unchanged ones included.
    pub set: Vec<(String, J)>,
    pub spec: ProjectSpec,
    pub project: GProject,
    pub score: Score,
    /// Hash of the built rooms, to tell apart rule changes that change nothing.
    pub fingerprint: u64,
}

/// The chosen variants, best first, and what the search saw.
#[derive(Clone, Debug)]
pub struct Variants {
    pub chosen: Vec<Variant>,
    pub axes: Vec<Axis>,
    /// Candidates built.
    pub tried: usize,
    /// Candidates refused, with the reason.
    pub failed: Vec<(Vec<(String, J)>, String)>,
    /// The distance the choice had to settle for.
    pub min_distance: f64,
}

/// The brief's value at a dotted path, `None` when absent.
fn at<'a>(v: &'a J, path: &str) -> Option<&'a J> {
    let mut x = v;
    for k in path.split('.') {
        x = x.get(k)?;
    }
    (!x.is_null()).then_some(x)
}

/// Whether the brief itself decides a rule (in `rules` or through its brief field).
fn decided(v: &J, id: &str) -> bool {
    v["rules"].get(id).is_some_and(|x| !x.is_null())
        || rules::def(id).and_then(|d| d.alias.as_deref()).is_some_and(|p| at(v, p).is_some())
}

/// The free axes of a brief.
pub fn axes(v: &J, s: &ProjectSpec, fixed: &[String]) -> Vec<Axis> {
    let mut out = vec![];
    let free = |id: &str| !decided(v, id) && !fixed.iter().any(|f| f == id);
    let massing = s.design.massing.name();
    if free("massing.form") {
        out.push(Axis {
            rule: "massing.form",
            values: Massing::NAMES.iter().map(|n| json!(n)).collect(),
            base: json!(massing),
        });
    }
    let snap = |x: f64| (x * 10.0).round() / 10.0;
    let numeric = |id: &'static str, base: f64, steps: &[f64]| -> Option<Axis> {
        let d = rules::def(id)?;
        let mut values: Vec<f64> = steps.iter().map(|s| snap(base + s)).filter(|x| d.contains(*x)).collect();
        if !values.contains(&base) {
            values.push(base);
        }
        values.sort_by(f64::total_cmp);
        values.dedup();
        (values.len() > 1).then(|| Axis { rule: id, values: values.iter().map(|x| json!(x)).collect(), base: json!(base) })
    };
    if free("grid.span") {
        out.extend(numeric("grid.span", s.grid, &[-1.2, 0.0, 1.2]));
    }
    if free("depth.band") && s.plate.is_none() && s.depth.is_none() {
        out.extend(numeric("depth.band", s.band_depth, &[-1.2, 0.0, 1.2]));
    }
    if free("facade.system") {
        let i = FacadeSystem::ALL.iter().position(|f| *f == s.design.facade).unwrap_or(0);
        out.push(Axis {
            rule: "facade.system",
            values: FacadeSystem::NAMES.iter().map(|n| json!(n)).collect(),
            base: json!(FacadeSystem::NAMES[i]),
        });
    }
    out
}

/// Distance of two value sets over the axes: per axis 1 for another option, the share of the
/// axis span for a number; the mean over the axes.
pub fn distance(axes: &[Axis], a: &[(String, J)], b: &[(String, J)]) -> f64 {
    if axes.is_empty() {
        return 0.0;
    }
    let get = |s: &[(String, J)], id: &str| s.iter().find(|x| x.0 == id).map(|x| x.1.clone());
    let mut sum = 0.0;
    for ax in axes {
        let (x, y) = (get(a, ax.rule).unwrap_or(ax.base.clone()), get(b, ax.rule).unwrap_or(ax.base.clone()));
        sum += match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => {
                let nums: Vec<f64> = ax.values.iter().filter_map(J::as_f64).collect();
                let span = nums.iter().cloned().fold(f64::MIN, f64::max) - nums.iter().cloned().fold(f64::MAX, f64::min);
                if span > 0.0 { ((x - y).abs() / span).min(1.0) } else { 0.0 }
            }
            _ => f64::from(u8::from(x != y)),
        };
    }
    sum / axes.len() as f64
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Every combination of axis values, the unchanged brief first, the rest shuffled by `seed`.
fn combinations(axes: &[Axis], seed: u64) -> Vec<Vec<(String, J)>> {
    let mut all: Vec<Vec<(String, J)>> = vec![vec![]];
    for ax in axes {
        all = all
            .into_iter()
            .flat_map(|c| {
                ax.values.iter().map(move |x| {
                    let mut c = c.clone();
                    c.push((ax.rule.to_string(), x.clone()));
                    c
                })
            })
            .collect();
    }
    let base: Vec<(String, J)> = axes.iter().map(|a| (a.rule.to_string(), a.base.clone())).collect();
    all.retain(|c| *c != base);
    let mut st = seed;
    for i in (1..all.len()).rev() {
        let j = (splitmix(&mut st) % (i as u64 + 1)) as usize;
        all.swap(i, j);
    }
    all.insert(0, base);
    all
}

fn fingerprint(p: &GProject) -> u64 {
    let mut h = DefaultHasher::new();
    format!("{:?}", p.footprint).hash(&mut h);
    p.copies.hash(&mut h);
    for s in &p.storeys {
        for r in &s.layout.rooms {
            for q in &r.polygon {
                ((q.0 * 100.0).round() as i64, (q.1 * 100.0).round() as i64).hash(&mut h);
            }
        }
        s.layout.openings.len().hash(&mut h);
    }
    h.finish()
}

/// The brief with the changed axes written as brief rules.
fn brief_of(v: &J, set: &[(String, J)], axes: &[Axis]) -> J {
    let mut b = v.clone();
    for (id, x) in set {
        if axes.iter().any(|a| a.rule == id.as_str() && a.base == *x) {
            continue;
        }
        if !b["rules"].is_object() {
            b["rules"] = J::Object(Map::new());
        }
        b["rules"][id] = x.clone();
    }
    b
}

/// Builds one candidate; `Err` says why it is not offered.
pub fn build(brief: J, set: Vec<(String, J)>) -> Result<Variant, String> {
    let spec = parse_spec(&brief).map_err(|e| e.to_string())?;
    let project = build_project(&spec).map_err(|e| e.to_string())?;
    let gates = check_project(&spec, &project);
    if !gates.passed() {
        return Err(format!("gates: {}", gates.failures.join("; ")));
    }
    let score = score(&spec, &project);
    let fingerprint = fingerprint(&project);
    Ok(Variant { brief, set, spec, project, score, fingerprint })
}

/// Up to `cfg.n` variants of a brief, best first, pairwise at least `cfg.min_distance` apart
/// where the pool allows.
pub fn variants(v: &J, cfg: &VaryCfg) -> Result<Variants, String> {
    let base = parse_spec(v).map_err(|e| e.to_string())?;
    let mut fixed = cfg.fixed.clone();
    fixed.extend(cfg.locks.iter().filter_map(|l| match &l.target {
        Target::Rule(id) => Some(id.clone()),
        _ => None,
    }));
    let axes = axes(v, &base, &fixed);
    let mut built: Vec<Variant> = vec![];
    let mut failed = vec![];
    let combos = combinations(&axes, cfg.seed);
    let tried = combos.len().min(cfg.pool.max(1));
    for set in combos.into_iter().take(tried) {
        match build(brief_of(v, &set, &axes), set.clone()) {
            Ok(x) => {
                let off: Vec<String> =
                    cfg.locks.iter().filter(|l| !l.holds(&x.brief, &x.spec, &x.project)).map(Lock::label).collect();
                if off.is_empty() {
                    built.push(x);
                } else {
                    failed.push((set, format!("breaks the lock on {}", off.join(", "))));
                }
            }
            Err(e) => failed.push((set, e)),
        }
    }
    // a candidate that misses a hard goal is offered only if every one does
    if built.iter().any(|x| x.score.hard_misses().is_empty()) {
        built.retain(|x| x.score.hard_misses().is_empty());
    }
    // stable: equal scores keep the pool order, the unchanged brief first
    built.sort_by(|a, b| a.score.compare(&b.score));
    let mut d = cfg.min_distance;
    let mut chosen: Vec<usize> = vec![];
    loop {
        chosen.clear();
        for (i, c) in built.iter().enumerate() {
            if chosen.len() == cfg.n {
                break;
            }
            let far = chosen.iter().all(|&j| {
                let o = &built[j];
                o.fingerprint != c.fingerprint && distance(&axes, &o.set, &c.set) + 1e-9 >= d
            });
            if far {
                chosen.push(i);
            }
        }
        if chosen.len() >= cfg.n.min(built.len()) || d < 1e-3 {
            break;
        }
        d /= 2.0;
    }
    let mut keep: Vec<Option<Variant>> = built.into_iter().map(Some).collect();
    let chosen: Vec<Variant> = chosen.into_iter().filter_map(|i| keep[i].take()).collect();
    let min_distance = pair_min(&axes, &chosen).unwrap_or(d);
    Ok(Variants { chosen, axes, tried, failed, min_distance })
}

/// Smallest pairwise distance of a choice; `None` for fewer than two.
pub fn pair_min(axes: &[Axis], c: &[Variant]) -> Option<f64> {
    let mut m: Option<f64> = None;
    for i in 0..c.len() {
        for j in i + 1..c.len() {
            let x = distance(axes, &c[i].set, &c[j].set);
            m = Some(m.map_or(x, |m| m.min(x)));
        }
    }
    m
}

/// The comparison the user and the assistant read: each variant's rule values, penalty per
/// tier, the rules it changes against the best, and why it ranks lower (the goals that cost it
/// more than the best).
pub fn compare(v: &Variants) -> J {
    let Some(best) = v.chosen.first() else {
        return json!({"variants": [], "note": "no variant could be built"});
    };
    let rows: Vec<J> = v
        .chosen
        .iter()
        .enumerate()
        .map(|(i, x)| {
            let differs: Map<String, J> = x
                .set
                .iter()
                .filter(|(id, val)| best.set.iter().find(|b| b.0 == *id).map(|b| &b.1) != Some(val))
                .map(|(id, val)| (id.clone(), val.clone()))
                .collect();
            let why_not: Vec<J> = best
                .score
                .contrast(&x.score)
                .into_iter()
                .filter(|c| c.2 > 0.0)
                .take(5)
                .map(|(rule, tier, d)| json!({"rule": rule, "tier": tier.name(), "costs_more": (d * 1000.0).round() / 1000.0}))
                .collect();
            let set: Map<String, J> = x.set.iter().cloned().collect();
            json!({"variant": i + 1, "rules": set, "changes_vs_best": differs,
                   "score": x.score.to_json()["tiers"], "penalty": x.score.to_json()["penalty"],
                   "why_not_best": why_not})
        })
        .collect();
    json!({"variants": rows, "tried": v.tried, "refused": v.failed.len(),
           "min_distance": (v.min_distance * 1000.0).round() / 1000.0})
}
