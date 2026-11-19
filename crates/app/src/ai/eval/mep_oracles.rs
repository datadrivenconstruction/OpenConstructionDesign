//! Network oracles: is what the model drew one connected system, or a heap of segments.
//! Runs touch where an end lies within 0.15 m of another run of the same system; a component
//! with a port (pump, unit, terminal) also joins the runs that end at it.

use super::case::GeomO;
use super::facts::{rx, sample, Cells, Comp, Facts, Run, P3};
use super::oracle::{in_range, no_capability, num, range_text, result, text, EvalCx, Oracle, OracleResult};
use std::collections::{BTreeMap, HashSet};

const TOUCH: f64 = 0.15;
const COMP_REACH: f64 = 0.9;
const SHAFT_MARGIN: f64 = 0.6;
const DEFAULT_EQUIPMENT: &str =
    "ahu|pump|valve|damper|fan|sprinkler|boiler|chiller|diffuser|grille|terminal|equipment|plumbing|mechanical|electrical";

pub fn build(g: &GeomO) -> Option<Box<dyn Oracle>> {
    ["mep_connected", "mep_free_ends", "mep_clashes", "served_rooms", "mep_kept", "mep_new_scope"]
        .contains(&g.kind.as_str())
        .then(|| Box::new(Mep(g.clone())) as Box<dyn Oracle>)
}

struct Mep(GeomO);

struct Dsu(Vec<usize>);

impl Dsu {
    fn find(&mut self, a: usize) -> usize {
        let mut r = a;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut x = a;
        while self.0[x] != r {
            let n = self.0[x];
            self.0[x] = r;
            x = n;
        }
        r
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

fn ends(r: &Run) -> Vec<(usize, P3)> {
    match (r.pts.first(), r.pts.last()) {
        (Some(a), Some(b)) if r.pts.len() >= 2 => vec![(0, *a), (1, *b)],
        _ => vec![],
    }
}

fn index(runs: &[Run]) -> Cells {
    let mut ix = Cells::new(0.5);
    for (i, r) in runs.iter().enumerate() {
        for w in r.pts.windows(2) {
            for p in sample(w[0], w[1], 0.3) {
                ix.add(p, i);
            }
        }
    }
    ix
}

fn is_equipment(c: &Comp, re: &regex::Regex, ports: &HashSet<i64>) -> bool {
    ports.contains(&c.id) || re.is_match(&c.family) || re.is_match(&c.category)
}

/// Runs joined into parts: per system, the lists of run ids of every part, biggest first.
pub fn parts(f: &Facts) -> BTreeMap<String, Vec<Vec<i64>>> {
    let runs = &f.runs;
    let ix = index(runs);
    let ported: Vec<&Comp> = f.comps.iter().filter(|c| f.port_comps.contains(&c.id)).collect();
    let mut dsu = Dsu((0..runs.len() + ported.len()).collect());
    for (i, r) in runs.iter().enumerate() {
        for (_, p) in ends(r) {
            for j in ix.near(p, TOUCH) {
                if j != i && runs[j].system == r.system {
                    dsu.union(i, j);
                }
            }
            for (k, c) in ported.iter().enumerate() {
                if super::facts::dist(c.at, p) <= COMP_REACH {
                    dsu.union(i, runs.len() + k);
                }
            }
        }
    }
    let mut by: BTreeMap<String, BTreeMap<usize, Vec<i64>>> = BTreeMap::new();
    for (i, r) in runs.iter().enumerate() {
        let root = dsu.find(i);
        by.entry(r.system.clone()).or_default().entry(root).or_default().push(r.id);
    }
    by.into_iter()
        .map(|(s, m)| {
            let mut v: Vec<Vec<i64>> = m.into_values().collect();
            v.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
            (s, v)
        })
        .collect()
}

/// An end on a connector linked to the run's own system, within `TOUCH`: a port off its
/// component's origin (a top feed) holds the end; a port of another system does not.
pub fn on_port(f: &Facts, system: &str, p: P3) -> bool {
    f.sys_ports.iter().any(|(s, q)| s == system && super::facts::dist(*q, p) <= TOUCH)
}

/// Ends that touch nothing: (run id, end, point), and the number of ends.
pub fn free_ends(f: &Facts, equipment: &regex::Regex) -> (Vec<(i64, usize, P3)>, usize) {
    let runs = &f.runs;
    let ix = index(runs);
    let near_comp = {
        let mut c = Cells::new(1.0);
        for (i, k) in f.comps.iter().enumerate() {
            if is_equipment(k, equipment, &f.port_comps) {
                c.add(k.at, i);
            }
        }
        c
    };
    let (mut free, mut total) = (vec![], 0);
    for (i, r) in runs.iter().enumerate() {
        for (k, p) in ends(r) {
            total += 1;
            let touched = ix.near(p, TOUCH).iter().any(|&j| j != i)
                || !near_comp.near(p, COMP_REACH).is_empty()
                || on_port(f, &r.system, p)
                || r.cap[k]
                || f.shafts.iter().any(|s| {
                    s.bbox.is_some_and(|b| {
                        p[0] >= b[0] - SHAFT_MARGIN
                            && p[0] <= b[2] + SHAFT_MARGIN
                            && p[1] >= b[1] - SHAFT_MARGIN
                            && p[1] <= b[3] + SHAFT_MARGIN
                    })
                });
            if !touched {
                free.push((r.id, k, p));
            }
        }
    }
    (free, total)
}

fn radius(r: &Run) -> f64 {
    let dn = r.dn.map_or(0.0, |d| if d < 5 { d as f64 } else { d as f64 / 1000.0 });
    r.w.max(r.h).max(dn) / 2.0
}

/// (run id, member index) pairs whose centre lines come closer than the run's radius + 0.12 m.
pub fn clashes(f: &Facts) -> Vec<(i64, usize)> {
    let mut ix = Cells::new(0.6);
    for (i, m) in f.members.iter().enumerate() {
        for p in sample(m.a, m.b, 0.3) {
            ix.add(p, i);
        }
    }
    let mut pairs = HashSet::new();
    for r in &f.runs {
        let reach = radius(r) + 0.12;
        for w in r.pts.windows(2) {
            for p in sample(w[0], w[1], 0.3) {
                for i in ix.near(p, reach) {
                    pairs.insert((r.id, i));
                }
            }
        }
    }
    let mut v: Vec<_> = pairs.into_iter().collect();
    v.sort_unstable();
    v
}

fn run_key(r: &Run) -> String {
    let p: Vec<String> = r.pts.iter().map(|p| format!("{:.0},{:.0},{:.0}", p[0] * 100.0, p[1] * 100.0, p[2] * 100.0)).collect();
    format!("{}|{}|{}", r.system, r.domain, p.join(";"))
}

impl Oracle for Mep {
    fn name(&self) -> String {
        format!("geom {}", self.0.kind)
    }

    fn check(&self, cx: &EvalCx) -> OracleResult {
        let p = &self.0.p;
        let name = self.name();
        let f = cx.facts;
        if !f.has("mep_run") {
            return no_capability(&name, "a model with network runs".into(), "no capability: the model has no table mep_run");
        }
        let equipment = match rx(&text(p, "equipment").unwrap_or_else(|| DEFAULT_EQUIPMENT.to_string())) {
            Ok(r) => r,
            Err(e) => return no_capability(&name, "a valid pattern".into(), &e),
        };
        match self.0.kind.as_str() {
            "mep_connected" => {
                let comp_max = num(p, "components_max").unwrap_or(1.0);
                let by = parts(f);
                let mut exp = format!("<= {comp_max} part(s) per system");
                let mut bad = vec![];
                for (sys, v) in &by {
                    if v.len() as f64 > comp_max {
                        // name the pieces cut off from the biggest one
                        let lost: Vec<String> = v[1..].iter().take(3).map(|part| format!("run {}", part[0])).collect();
                        bad.push(format!("{sys}: {} parts, cut off: {}", v.len(), lost.join(", ")));
                    }
                }
                if let Some(min) = num(p, "system_min") {
                    exp += &format!(", at least {min} system(s)");
                    if (by.len() as f64) < min {
                        bad.push(format!("{} system(s)", by.len()));
                    }
                }
                let mut shown = format!("{} system(s), {} run(s)", by.len(), f.runs.len());
                if let Some(share) = num(p, "ends_min") {
                    let (free, total) = free_ends(f, &equipment);
                    let got = if total == 0 { 0.0 } else { 1.0 - free.len() as f64 / total as f64 };
                    exp += &format!(", ends connected >= {share}");
                    shown += &format!(", ends connected {got:.3}");
                    if got + 1e-9 < share {
                        bad.push(format!("ends connected {got:.3}"));
                    }
                }
                if f.runs.is_empty() {
                    bad.push("no runs".into());
                }
                if !bad.is_empty() {
                    shown += &format!("; {}", bad.join("; "));
                }
                result(&name, exp, shown, bad.is_empty())
            }
            "mep_free_ends" => {
                let max = num(p, "max").unwrap_or(0.0);
                let (free, total) = free_ends(f, &equipment);
                let named: Vec<String> = free
                    .iter()
                    .take(3)
                    .map(|(id, k, q)| format!("run {id} end {k} at ({:.1}, {:.1}, {:.1})", q[0], q[1], q[2]))
                    .collect();
                let shown = format!("{} free of {total} ends{}", free.len(), if named.is_empty() { String::new() } else { format!(": {}", named.join("; ")) });
                result(&name, format!("<= {max}"), shown, free.len() as f64 <= max)
            }
            "mep_clashes" => {
                let max = num(p, "max").unwrap_or(0.0);
                let c = clashes(f);
                let first = c.first().map(|(r, m)| format!(", first: run {r} with member {m}")).unwrap_or_default();
                result(&name, format!("<= {max}"), format!("{} clashing pair(s){first}", c.len()), c.len() as f64 <= max)
            }
            "served_rooms" => {
                let (Some(terminal), share_min) = (text(p, "terminal"), num(p, "share_min").unwrap_or(1.0)) else {
                    return no_capability(&name, "a terminal pattern".into(), "missing parameter terminal");
                };
                let pick = |k: &str| text(p, k).filter(|s| !s.is_empty()).map(|s| rx(&s)).transpose();
                let (term, only, skip) = match (rx(&terminal), pick("rooms"), pick("exclude")) {
                    (Ok(a), Ok(b), Ok(c)) => (a, b, c),
                    (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return no_capability(&name, "valid patterns".into(), &e),
                };
                let reach = num(p, "radius").unwrap_or(6.0);
                let terms: Vec<&Comp> = f.comps.iter().filter(|c| term.is_match(&c.family) || term.is_match(&c.category)).collect();
                let rooms: Vec<_> = f
                    .rooms
                    .iter()
                    .filter(|r| only.as_ref().is_none_or(|o| o.is_match(&r.name)) && !skip.as_ref().is_some_and(|s| s.is_match(&r.name)))
                    .collect();
                let unserved: Vec<&str> = rooms
                    .iter()
                    .filter(|r| !terms.iter().any(|c| c.level == r.level && (c.at[0] - r.at[0]).hypot(c.at[1] - r.at[1]) <= reach))
                    .map(|r| r.name.as_str())
                    .collect();
                let share = if rooms.is_empty() { 0.0 } else { 1.0 - unserved.len() as f64 / rooms.len() as f64 };
                let shown = format!(
                    "{} of {} rooms served ({share:.3}){}",
                    rooms.len() - unserved.len(),
                    rooms.len(),
                    if unserved.is_empty() { String::new() } else { format!("; without: {}", unserved.iter().take(6).copied().collect::<Vec<_>>().join(", ")) }
                );
                result(&name, format!(">= {share_min} of rooms within {reach} m of a terminal"), shown, !rooms.is_empty() && share + 1e-9 >= share_min)
            }
            "mep_kept" | "mep_new_scope" => {
                let Some(b) = cx.before else {
                    return no_capability(&name, "a base model".into(), "case has no base model");
                };
                let old: HashSet<String> = b.runs.iter().map(run_key).collect();
                let now: HashSet<String> = f.runs.iter().map(run_key).collect();
                if self.0.kind == "mep_kept" {
                    let min = num(p, "min").unwrap_or(1.0);
                    let kept = old.iter().filter(|k| now.contains(*k)).count();
                    let share = if old.is_empty() { 1.0 } else { kept as f64 / old.len() as f64 };
                    result(&name, range_text(Some(min), None), format!("{kept} of {} runs unchanged ({share:.3})", old.len()), in_range(share, Some(min), None))
                } else {
                    let new: Vec<&Run> = f.runs.iter().filter(|r| !old.contains(&run_key(r))).collect();
                    let systems: HashSet<&str> = new.iter().map(|r| r.system.as_str()).collect();
                    let levels: HashSet<i64> = new.iter().map(|r| r.level).collect();
                    let (smax, lmax, nmin) = (num(p, "systems_max").unwrap_or(1.0), num(p, "levels_max").unwrap_or(1.0), num(p, "new_min").unwrap_or(1.0));
                    let ok = new.len() as f64 >= nmin && systems.len() as f64 <= smax && levels.len() as f64 <= lmax;
                    let exp = format!(">= {nmin} new run(s) in <= {smax} system(s) on <= {lmax} level(s)");
                    result(&name, exp, format!("{} new run(s) in {} system(s) on {} level(s)", new.len(), systems.len(), levels.len()), ok)
                }
            }
            _ => unreachable!("build admits only known kinds"),
        }
    }
}
