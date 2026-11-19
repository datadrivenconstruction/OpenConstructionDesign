// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Why a brief cannot be built: the smallest set of its own demands that cannot hold together,
//! and the nearest relaxations that would let it build. A demand is a rule the brief sets
//! (under `rules` or through a rule's brief field), a row of its room programme, its storey
//! count, or a lock. A brief is feasible when it parses, builds, passes the gates, misses no
//! hard goal and keeps the locks it carries.
//!
//! The set is found by removing demands, in halves first and then one by one, until every
//! remaining demand is needed: dropping any one of them makes the brief feasible. Each rule in
//! the set is then relaxed to the feasible value nearest to the one asked, within its range,
//! and offered as a [`RuleFix`].

use super::locks::{of_brief, Lock};
use super::rules;
use super::score::score;
use super::why::RuleFix;
use super::{build_project, check_project, parse_spec};
use serde_json::{json, Map, Value as J};

/// One demand of a brief.
#[derive(Clone, Debug, PartialEq)]
pub enum Demand {
    /// A rule value, with the brief path it was written at.
    Rule { id: String, path: Vec<String> },
    /// A row of a room list (`rooms`, `ground_rooms`, `program.spaces`).
    Row { list: Vec<String>, index: usize },
    /// A top-level field that is no rule (`floors`, `area`, `apartments`).
    Field(String),
    /// The lock at this index of `locks`.
    Lock(usize),
}

impl Demand {
    pub fn label(&self, brief: &J) -> String {
        match self {
            Demand::Rule { id, .. } => format!("rule {id}"),
            Demand::Row { list, index } => {
                let row = get(brief, list).get(*index).cloned().unwrap_or(J::Null);
                let what = row["kind"].as_str().or(row["name"].as_str()).or(row["id"].as_str()).unwrap_or("row");
                format!("{}[{index}] {what}", list.join("."))
            }
            Demand::Field(k) => format!("field {k}"),
            Demand::Lock(i) => of_brief(brief)
                .ok()
                .and_then(|l| l.get(*i).map(Lock::label))
                .map_or(format!("lock {i}"), |l| format!("lock on {l}")),
        }
    }
}

fn get<'a>(v: &'a J, path: &[String]) -> &'a J {
    path.iter().fold(v, |x, k| &x[k.as_str()])
}

fn remove_at(v: &mut J, path: &[String]) {
    let Some((last, head)) = path.split_last() else { return };
    let mut at = v;
    for k in head {
        match at.get_mut(k.as_str()) {
            Some(x) => at = x,
            None => return,
        }
    }
    if let Some(o) = at.as_object_mut() {
        o.remove(last);
    }
}

/// Every demand of a brief, in a fixed order.
pub fn demands(v: &J) -> Vec<Demand> {
    let mut out = vec![];
    for id in v["rules"].as_object().into_iter().flat_map(|o| o.keys()) {
        if rules::def(id).is_some() {
            out.push(Demand::Rule { id: id.clone(), path: vec!["rules".into(), id.clone()] });
        }
    }
    for d in rules::defs() {
        let Some(path) = d.alias.as_deref().filter(|p| !p.contains("[]") && *p != "footprint.polygon") else {
            continue;
        };
        let path: Vec<String> = path.split('.').map(str::to_string).collect();
        if !get(v, &path).is_null() && !out.iter().any(|x| matches!(x, Demand::Rule { id, .. } if *id == d.id)) {
            out.push(Demand::Rule { id: d.id.clone(), path });
        }
    }
    for list in [vec!["rooms"], vec!["ground_rooms"], vec!["program", "spaces"]] {
        let list: Vec<String> = list.into_iter().map(str::to_string).collect();
        let n = get(v, &list).as_array().map_or(0, Vec::len);
        out.extend((0..n).map(|index| Demand::Row { list: list.clone(), index }));
    }
    for k in ["floors", "area", "apartments"] {
        if !v[k].is_null() {
            out.push(Demand::Field(k.into()));
        }
    }
    let locks = v["locks"].as_array().map_or(0, Vec::len);
    out.extend((0..locks).map(Demand::Lock));
    out
}

/// The brief keeping only the demands in `keep`.
pub fn restrict(v: &J, all: &[Demand], keep: &[Demand]) -> J {
    let mut b = v.clone();
    let mut drop_rows: Vec<(Vec<String>, usize)> = vec![];
    let mut drop_locks: Vec<usize> = vec![];
    for d in all.iter().filter(|d| !keep.contains(d)) {
        match d {
            Demand::Rule { path, .. } => remove_at(&mut b, path),
            Demand::Field(k) => remove_at(&mut b, std::slice::from_ref(k)),
            Demand::Row { list, index } => drop_rows.push((list.clone(), *index)),
            Demand::Lock(i) => drop_locks.push(*i),
        }
    }
    drop_rows.sort_by(|a, b| b.1.cmp(&a.1));
    for (list, i) in drop_rows {
        let mut empty = false;
        if let Some(a) = list.iter().try_fold(&mut b, |x, k| x.get_mut(k.as_str())).and_then(J::as_array_mut) {
            if i < a.len() {
                a.remove(i);
            }
            empty = a.is_empty();
        }
        if empty {
            remove_at(&mut b, &list);
        }
    }
    if let Some(a) = b.get_mut("locks").and_then(J::as_array_mut) {
        let kept: Vec<J> = a.iter().enumerate().filter(|(i, _)| !drop_locks.contains(i)).map(|x| x.1.clone()).collect();
        *a = kept;
    }
    if b["rules"].as_object().is_some_and(Map::is_empty) {
        remove_at(&mut b, &["rules".to_string()]);
    }
    b
}

/// `Ok` when the brief builds, passes the gates, misses no hard goal and keeps its locks; else
/// the first reason.
pub fn feasible(v: &J) -> Result<(), String> {
    let locks = of_brief(v)?;
    let s = parse_spec(v).map_err(|e| e.to_string())?;
    let p = build_project(&s).map_err(|e| e.to_string())?;
    let g = check_project(&s, &p);
    if !g.passed() {
        return Err(format!("gates: {}", g.failures.join("; ")));
    }
    let sc = score(&s, &p);
    if let Some(c) = sc.hard_misses().first() {
        return Err(format!("hard goal {} missed: measured {:.3} against {}", c.rule, c.measured, c.target));
    }
    if let Some(l) = locks.iter().find(|l| !l.holds(v, &s, &p)) {
        return Err(format!("the lock on {} breaks", l.label()));
    }
    Ok(())
}

/// The conflict of an infeasible brief.
#[derive(Clone, Debug)]
pub struct Conflict {
    /// Why the whole brief fails.
    pub error: String,
    /// The demands that cannot hold together; dropping any one makes the brief feasible.
    pub set: Vec<Demand>,
    pub labels: Vec<String>,
    /// Nearest feasible values of the rules in the set, each alone enough.
    pub fixes: Vec<RuleFix>,
    /// Builds the search took.
    pub builds: usize,
}

impl Conflict {
    pub fn to_json(&self) -> J {
        json!({"error": self.error, "conflict": self.labels,
               "relax": self.fixes.iter().map(RuleFix::to_json).collect::<Vec<_>>(),
               "note": "dropping any one demand of the conflict lets the brief build; each relax alone does too"})
    }
}

struct Search<'a> {
    v: &'a J,
    all: Vec<Demand>,
    builds: usize,
}

impl Search<'_> {
    fn ok(&mut self, keep: &[Demand]) -> bool {
        self.builds += 1;
        feasible(&restrict(self.v, &self.all, keep)).is_ok()
    }

    /// Removes demands while the rest stays infeasible: halves, then quarters, down to one by
    /// one, repeated until a whole one-by-one pass removes nothing.
    fn minimise(&mut self) -> Vec<Demand> {
        let mut s = self.all.clone();
        let mut k = (s.len() / 2).max(1);
        loop {
            let mut removed = false;
            let mut i = 0;
            while i < s.len() {
                let cand: Vec<Demand> = s.iter().enumerate().filter(|(j, _)| *j < i || *j >= i + k).map(|x| x.1.clone()).collect();
                if cand.len() < s.len() && !self.ok(&cand) {
                    s = cand;
                    removed = true;
                } else {
                    i += k;
                }
            }
            if k > 1 {
                k = (k / 2).max(1);
            } else if !removed {
                return s;
            }
        }
    }

    /// The brief with every demand, the rule `id` set to `x` at `path`.
    fn with(&self, path: &[String], id: &str, x: f64) -> J {
        let mut b = self.v.clone();
        let d = rules::def(id).expect("a known rule");
        let val = d.value_json(x);
        let mut at = &mut b;
        for k in &path[..path.len() - 1] {
            if !at[k.as_str()].is_object() {
                at[k.as_str()] = J::Object(Map::new());
            }
            at = &mut at[k.as_str()];
        }
        let last = path[path.len() - 1].as_str();
        // a goal written as {value, weight, tier} keeps its weight and tier
        if at[last].is_object() {
            at[last]["value"] = val;
        } else {
            at[last] = val;
        }
        b
    }

    fn fits(&mut self, path: &[String], id: &str, x: f64) -> bool {
        self.builds += 1;
        feasible(&self.with(path, id, x)).is_ok()
    }

    /// The feasible value of a rule nearest to the asked one, the rest of the brief unchanged.
    fn relax(&mut self, id: &str, path: &[String]) -> Option<f64> {
        let d = rules::def(id)?;
        let raw = get(self.v, path);
        let asked = d.parse_value(if raw.is_object() { &raw["value"] } else { raw })?;
        let mut probes: Vec<f64> = vec![];
        if !d.options.is_empty() || d.flag {
            probes.extend((0..=d.max as usize).map(|i| i as f64));
        } else {
            probes.extend(d.default);
            probes.push(d.min);
            probes.push(d.max);
        }
        probes.retain(|x| (*x - asked).abs() > 1e-9);
        probes.sort_by(|a, b| (a - asked).abs().total_cmp(&(b - asked).abs()));
        probes.dedup();
        let far = probes.into_iter().find(|x| self.fits(path, id, *x))?;
        if d.flag || !d.options.is_empty() {
            return Some(far);
        }
        // bisect between the asked value and the feasible probe
        let (mut bad, mut good) = (asked, far);
        let step = if d.int { 1.0 } else { ((d.max - d.min) / 1000.0).max(1e-3) };
        for _ in 0..8 {
            if (good - bad).abs() <= step {
                break;
            }
            let mut mid = (bad + good) / 2.0;
            if d.int {
                mid = mid.round();
            } else {
                mid = (mid / step).round() * step;
            }
            if !d.contains(mid) || mid == bad || mid == good {
                break;
            }
            if self.fits(path, id, mid) {
                good = mid;
            } else {
                bad = mid;
            }
        }
        Some(good)
    }
}

/// The conflict of a brief, `None` when it is feasible.
pub fn find(v: &J) -> Option<Conflict> {
    let error = feasible(v).err()?;
    let mut s = Search { v, all: demands(v), builds: 1 };
    let set = s.minimise();
    let labels = set.iter().map(|d| d.label(v)).collect();
    let mut fixes = vec![];
    for d in &set {
        if let Demand::Rule { id, path } = d {
            if let Some(x) = s.relax(id, path) {
                let value = rules::def(id).map_or(json!(x), |r| r.value_json(x));
                fixes.extend(RuleFix::keyed(id, value, "fix_conflict", vec![]));
            }
        }
    }
    Some(Conflict { error, set, labels, fixes, builds: s.builds })
}
