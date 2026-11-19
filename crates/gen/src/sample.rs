// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Option sampling for `generate_options`: which parameter values each of up to [`MAX_OPTIONS`]
//! options gets (grid, Latin hypercube or random, all seed-deterministic), and the pure
//! generation of all options on a small thread pool. No model access.

use crate::params::bad;
use crate::{variant_of, GenError, GenOut, Generator, ParamKind};
use serde_json::{Map, Value as J};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Options one request may produce.
pub const MAX_OPTIONS: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub enum Axis {
    Range { min: f64, max: f64 },
    List(Vec<J>),
}

/// Parameters to vary, in a stable (name) order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vary(pub Vec<(String, Axis)>);

impl Vary {
    /// `{"param": {"min": a, "max": b} | [v1, v2, ...]}`.
    pub fn parse(v: &J) -> Result<Vary, GenError> {
        let mut out = vec![];
        match v {
            J::Null => {}
            J::Object(o) => {
                for (k, a) in o {
                    let axis = match a {
                        J::Array(l) if !l.is_empty() => Axis::List(l.clone()),
                        J::Object(_) => {
                            let lo = a["min"].as_f64().filter(|x| x.is_finite());
                            let hi = a["max"].as_f64().filter(|x| x.is_finite());
                            match (lo, hi) {
                                (Some(min), Some(max)) if min <= max => Axis::Range { min, max },
                                _ => return Err(bad(k, "range needs finite min <= max")),
                            }
                        }
                        _ => return Err(bad(k, "must be {min, max} or a non-empty list")),
                    };
                    out.push((k.clone(), axis));
                }
            }
            _ => return Err(bad("vary", "must be an object")),
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Vary(out))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sampler {
    Grid,
    Lhs,
    Random,
}

impl Sampler {
    pub fn parse(s: Option<&str>) -> Result<Sampler, GenError> {
        match s {
            None | Some("lhs") => Ok(Sampler::Lhs),
            Some("grid") => Ok(Sampler::Grid),
            Some("random") => Ok(Sampler::Random),
            Some(_) => Err(bad("sampler", "must be one of grid, lhs, random")),
        }
    }
}

/// SplitMix64: small, fast, fully determined by the seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        ((self.unit() * n as f64) as usize).min(n.saturating_sub(1))
    }
}

/// Generator seed of option `i`: one seed per option, so the variant criterion rotates too.
pub fn option_seed(seed: u64, i: usize) -> u64 {
    seed.wrapping_add(i as u64)
}

fn grid_coords(vary: &Vary, n: usize) -> Vec<Vec<f64>> {
    let ranges = vary
        .0
        .iter()
        .filter(|a| matches!(a.1, Axis::Range { .. }))
        .count() as u32;
    let fixed: usize = vary
        .0
        .iter()
        .map(|a| match &a.1 {
            Axis::List(l) => l.len(),
            Axis::Range { .. } => 1,
        })
        .product();
    let mut k = 1usize;
    while ranges > 0 && fixed * k.pow(ranges) < n {
        k += 1;
    }
    let levels: Vec<usize> = vary
        .0
        .iter()
        .map(|a| match &a.1 {
            Axis::List(l) => l.len(),
            Axis::Range { .. } => k,
        })
        .collect();
    let total: usize = levels.iter().product();
    (0..n)
        .map(|i| {
            // cells spread evenly over the grid; a grid smaller than n repeats (dropped later)
            let mut idx = if total >= n { i * total / n } else { i % total };
            levels
                .iter()
                .zip(&vary.0)
                .map(|(&l, a)| {
                    let at = idx % l;
                    idx /= l;
                    match a.1 {
                        Axis::Range { .. } if l == 1 => 0.5,
                        Axis::Range { .. } => at as f64 / (l - 1) as f64,
                        Axis::List(_) => (at as f64 + 0.5) / l as f64,
                    }
                })
                .collect()
        })
        .collect()
}

/// Parameter overlays of `min(n, MAX_OPTIONS)` options.
pub fn sample(vary: &Vary, sampler: Sampler, n: usize, seed: u64) -> Vec<Map<String, J>> {
    let n = n.min(MAX_OPTIONS);
    let d = vary.0.len();
    let mut rng = Rng(seed ^ 0x6f70_7469_6f6e_7321);
    let coords: Vec<Vec<f64>> = match sampler {
        Sampler::Random => (0..n)
            .map(|_| (0..d).map(|_| rng.unit()).collect())
            .collect(),
        Sampler::Lhs => {
            let mut c = vec![vec![0.0; d]; n];
            for j in 0..d {
                let mut perm: Vec<usize> = (0..n).collect();
                for i in (1..n).rev() {
                    perm.swap(i, rng.below(i + 1));
                }
                for i in 0..n {
                    c[i][j] = (perm[i] as f64 + rng.unit()) / n as f64;
                }
            }
            c
        }
        Sampler::Grid => grid_coords(vary, n),
    };
    coords
        .iter()
        .map(|c| {
            let mut m = Map::new();
            for ((name, axis), &t) in vary.0.iter().zip(c) {
                let v = match axis {
                    Axis::Range { min, max } => J::from(min + t * (max - min)),
                    Axis::List(l) => {
                        l[((t * l.len() as f64) as usize).min(l.len() - 1)].clone()
                    }
                };
                m.insert(name.clone(), v);
            }
            m
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// Position in the request, 0-based; stable across replays.
    pub index: usize,
    pub seed: u64,
    pub params: J,
    pub criterion: String,
    pub out: GenOut,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Batch {
    pub options: Vec<Candidate>,
    /// Indices dropped because an earlier option gave the same result.
    pub duplicates: Vec<usize>,
    pub failed: Vec<(usize, String)>,
}

fn overlay(g: &dyn Generator, base: &J, over: &Map<String, J>) -> Result<J, GenError> {
    let mut p = match base {
        J::Null => Map::new(),
        J::Object(o) => o.clone(),
        _ => return Err(bad("params", "must be an object")),
    };
    for (k, v) in over {
        let def = g
            .params()
            .iter()
            .find(|d| d.name == k)
            .ok_or_else(|| bad(k, "unknown parameter"))?;
        let v = match (def.kind, v.as_f64()) {
            (ParamKind::Count, Some(x)) => J::from(x.round()),
            (ParamKind::Length | ParamKind::Area | ParamKind::Ratio, Some(x)) => {
                // six decimals: the same sample always prints and hashes the same
                J::from((x * 1e6).round() / 1e6)
            }
            _ => v.clone(),
        };
        p.insert(k.clone(), v);
    }
    Ok(J::Object(p))
}

fn criterion_of(g: &dyn Generator, seed: u64, over: &Map<String, J>) -> String {
    let mut parts: Vec<String> = vec![];
    if let Some(v) = g.variants().get(variant_of(seed)) {
        parts.push(v.to_string());
    }
    for (k, v) in over {
        parts.push(format!("{k} = {v}"));
    }
    parts.join("; ")
}

/// Builds `min(n, MAX_OPTIONS)` options of `g` on up to `workers` threads. The result depends
/// only on the arguments, never on the number of workers or their timing.
pub fn generate(
    g: &dyn Generator,
    base: &J,
    vary: &Vary,
    sampler: Sampler,
    n: usize,
    seed: u64,
    workers: usize,
) -> Result<Batch, GenError> {
    let overlays = sample(vary, sampler, n, seed);
    let mut jobs: Vec<(u64, J, String)> = Vec::with_capacity(overlays.len());
    for (i, over) in overlays.iter().enumerate() {
        let s = option_seed(seed, i);
        jobs.push((s, overlay(g, base, over)?, criterion_of(g, s, over)));
    }
    let results: Mutex<Vec<Option<Result<GenOut, GenError>>>> = Mutex::new(vec![None; jobs.len()]);
    let next = AtomicUsize::new(0);
    let w = workers.clamp(1, jobs.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..w {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(job) = jobs.get(i) else { break };
                let r = g.build(&job.1, job.0);
                results.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
            });
        }
    });
    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    let mut batch = Batch::default();
    for (i, (r, (s, params, criterion))) in results.into_iter().zip(jobs).enumerate() {
        match r {
            Some(Ok(out)) => {
                if batch.options.iter().any(|c| c.out == out) {
                    batch.duplicates.push(i);
                } else {
                    batch.options.push(Candidate {
                        index: i,
                        seed: s,
                        params,
                        criterion,
                        out,
                    });
                }
            }
            Some(Err(e)) => batch.failed.push((i, e.to_string())),
            None => batch.failed.push((i, "not built".to_string())),
        }
    }
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ParamDef, ParamKind};
    use serde_json::json;

    struct Probe;

    static PARAMS: [ParamDef; 2] = [
        ParamDef {
            name: "w",
            kind: ParamKind::Length,
            min: 1.0,
            max: 10.0,
            default: 2.0,
            help: "",
        },
        ParamDef {
            name: "n",
            kind: ParamKind::Count,
            min: 1.0,
            max: 9.0,
            default: 3.0,
            help: "",
        },
    ];

    impl Generator for Probe {
        fn id(&self) -> &'static str {
            "probe"
        }
        fn params(&self) -> &'static [ParamDef] {
            &PARAMS
        }
        fn build(&self, p: &J, seed: u64) -> Result<GenOut, GenError> {
            let w = p["w"].as_f64().unwrap_or(2.0);
            if w > 10.0 {
                return Err(bad("w", "too wide"));
            }
            Ok(GenOut::Family {
                def: format!("w={w} n={} v={}", p["n"], variant_of(seed)),
            })
        }
        fn variants(&self) -> &'static [&'static str] {
            &["a", "b", "c"]
        }
    }

    fn vary(v: J) -> Vary {
        Vary::parse(&v).unwrap()
    }

    #[test]
    fn four_options_deterministic_by_seed() {
        let v = vary(json!({"w": {"min": 2.0, "max": 8.0}, "n": {"min": 1, "max": 9}}));
        let run = |seed, workers| generate(&Probe, &J::Null, &v, Sampler::Lhs, 4, seed, workers).unwrap();
        let a = run(7, 1);
        assert_eq!(a.options.len(), 4);
        assert_eq!(a, run(7, 4), "same seed, any pool size: same options");
        assert_ne!(a.options, run(8, 2).options);
        for c in &a.options {
            assert!(c.params["n"].as_f64().unwrap().fract() == 0.0, "counts are whole");
        }
    }

    #[test]
    fn duplicate_options_dropped() {
        // one value, seeds 1..4: the fourth seed wraps to the first variant
        let v = vary(json!({"w": [3.0]}));
        let b = generate(&Probe, &json!({}), &v, Sampler::Grid, 4, 1, 3).unwrap();
        assert_eq!(b.options.len(), 3);
        assert_eq!(b.duplicates, vec![3]);
    }

    #[test]
    fn n_over_max_is_capped() {
        let v = vary(json!({"w": {"min": 1.0, "max": 9.0}}));
        assert_eq!(sample(&v, Sampler::Random, 12, 1).len(), MAX_OPTIONS);
        assert!(sample(&v, Sampler::Lhs, 0, 1).is_empty());
    }

    #[test]
    fn lhs_puts_one_value_in_each_stratum() {
        let v = vary(json!({"t": {"min": 0.0, "max": 1.0}}));
        let mut cells: Vec<usize> = sample(&v, Sampler::Lhs, 8, 42)
            .iter()
            .map(|m| (m["t"].as_f64().unwrap() * 8.0) as usize)
            .collect();
        cells.sort();
        assert_eq!(cells, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn grid_includes_range_ends() {
        let v = vary(json!({"t": {"min": 1.0, "max": 3.0}}));
        let got: Vec<f64> = sample(&v, Sampler::Grid, 3, 0)
            .iter()
            .map(|m| m["t"].as_f64().unwrap())
            .collect();
        assert_eq!(got, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn bad_requests_are_refused_with_reason() {
        assert!(Vary::parse(&json!({"w": {"min": 3.0, "max": 1.0}})).is_err());
        assert!(Sampler::parse(Some("sobol")).is_err());
        let v = vary(json!({"depth": [1.0]}));
        let e = generate(&Probe, &J::Null, &v, Sampler::Grid, 1, 1, 1).unwrap_err();
        assert!(e.to_string().contains("unknown parameter"));
        let v = vary(json!({"w": [20.0]}));
        let b = generate(&Probe, &J::Null, &v, Sampler::Grid, 1, 1, 1).unwrap();
        assert!(b.options.is_empty());
        assert!(b.failed[0].1.contains("too wide"), "{:?}", b.failed);
    }
}
