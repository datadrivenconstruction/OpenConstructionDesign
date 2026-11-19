// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Self-critique loop: build the spec, render snapshots, let a critic score them and answer
//! with a merge patch on the spec, rebuild, and keep the best round. Rendering and the critic
//! are traits, so the app plugs in its viewport and a model while tests use plain code.

use super::{build_project, check_project, merge_patch, parse_spec, Design, FacadeSystem, GProject, Massing};
use super::why::{fixes_of, spec_patch, RuleFix};
use serde_json::{json, Value as J};

/// One rendered view of a built project.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// `axo`, `plan:0`, `elev:south` and the like.
    pub view: String,
    pub png: Vec<u8>,
}

pub trait Renderer {
    fn render(&mut self, p: &GProject) -> Result<Vec<Snapshot>, String>;
}

/// For a critic that reads only the spec and the facts (such as [`RuleCritic`]): no images.
pub struct NoShots;

impl Renderer for NoShots {
    fn render(&mut self, _p: &GProject) -> Result<Vec<Snapshot>, String> {
        Ok(vec![])
    }
}

/// Scores in 0..=10 by criterion and the rule changes proposed; `patch` is those changes as a
/// merge patch on the spec. An empty patch ends the loop.
#[derive(Clone, Debug, PartialEq)]
pub struct Critique {
    pub scores: Vec<(String, f64)>,
    pub patch: J,
    pub notes: String,
    /// Each finding as a rule and a value in its range.
    pub rules: Vec<RuleFix>,
}

impl Critique {
    pub fn total(&self) -> f64 {
        if self.scores.is_empty() {
            return 0.0;
        }
        self.scores.iter().map(|s| s.1).sum::<f64>() / self.scores.len() as f64
    }
}

pub trait Critic {
    fn critique(&mut self, spec: &J, facts: &J, shots: &[Snapshot]) -> Result<Critique, String>;
}

#[derive(Clone, Copy, Debug)]
pub struct LoopCfg {
    pub max_rounds: u32,
    /// Stop once the mean score reaches this.
    pub target: f64,
}

impl Default for LoopCfg {
    fn default() -> Self {
        LoopCfg {
            max_rounds: 3,
            target: 8.0,
        }
    }
}

/// The rule that bounds the rounds.
pub const ROUNDS_RULE: &str = "layout.refine_rounds";

impl LoopCfg {
    /// The rounds the brief's rules allow (`layout.refine_rounds`, 0 = no critique).
    pub fn of(spec: &J) -> Result<LoopCfg, String> {
        let s = parse_spec(spec).map_err(|e| e.to_string())?;
        Ok(LoopCfg { max_rounds: s.rules.get(ROUNDS_RULE).round() as u32, ..LoopCfg::default() })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Round {
    pub spec: J,
    pub score: f64,
    pub critique: Option<Critique>,
    /// Why the patched spec of this round did not build, if it did not.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Refined {
    pub best: J,
    pub score: f64,
    pub rounds: Vec<Round>,
}

/// The criteria a critic scores, in the words the model reads.
pub const RUBRIC: [(&str, &str); 5] = [
    ("massing", "volume reads as intentional: wings, steps or a podium where the program allows"),
    ("facade", "a clear facade system with rhythm; no blank or random walls"),
    ("entrance", "the entrance is legible: canopy, paving to the door"),
    ("site", "the building sits in a site: paving, trees, terraces"),
    ("brief", "the program and storeys match the brief"),
];

/// The critic's instruction: rubric, the facts of the build and the patch it must answer with.
pub fn prompt(spec: &J, facts: &J) -> String {
    let rubric: Vec<String> = RUBRIC.iter().map(|(k, v)| format!("- {k}: {v}")).collect();
    format!(
        "Score the rendered building 0-10 on each criterion, then reply with JSON \
         {{\"scores\":{{...}},\"rules\":{{...}},\"notes\":\"...\"}} where rules maps a generation \
         rule id to a new value within its range (only the rules to change; {{}} when nothing \
         should change). A field the rules cannot say may go in \"patch\", a merge patch on the \
         spec.\n{}\nspec: {spec}\nfacts: {facts}",
        rubric.join("\n")
    )
}

/// Reads a critic's JSON reply into a critique.
pub fn parse_reply(text: &str) -> Result<Critique, String> {
    let start = text.find('{').ok_or("no JSON object in the reply")?;
    let end = text.rfind('}').ok_or("no JSON object in the reply")?;
    let v: J = serde_json::from_str(&text[start..=end]).map_err(|e| e.to_string())?;
    let scores = v["scores"]
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, s)| s.as_f64().map(|x| (k.clone(), x.clamp(0.0, 10.0))))
                .collect()
        })
        .unwrap_or_default();
    let mut notes = v["notes"].as_str().unwrap_or_default().to_string();
    let mut fixes = vec![];
    for (id, x) in v["rules"].as_object().into_iter().flatten() {
        match RuleFix::keyed(id, x.clone(), "fix_critic", vec![]) {
            Ok(f) => fixes.push(f),
            // a value outside the rule's range is not applied, and the reply says so
            Err(e) => notes.push_str(&format!(" [refused {e}]")),
        }
    }
    let mut patch = spec_patch(&fixes);
    if v["patch"].is_object() {
        merge_patch(&mut patch, &v["patch"]);
        fixes.extend(fixes_of(&v["patch"], "critic"));
    }
    Ok(Critique {
        scores,
        patch,
        notes: notes.trim().to_string(),
        rules: fixes,
    })
}

/// What the critic is told besides the pictures: counts, sizes and the gate advice.
pub fn facts(p: &GProject) -> J {
    let fixtures: usize = p.storeys.iter().map(|s| s.layout.fixtures.len()).sum();
    let metrics: serde_json::Map<String, J> = p.metrics.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    json!({
        "storeys": p.storeys.len(),
        "footprint": [p.footprint.0, p.footprint.1],
        "roofs": 1 + p.extra_roofs.len(),
        "fixtures": fixtures,
        "metrics": metrics,
    })
}

fn build(spec: &J) -> Result<GProject, String> {
    let s = parse_spec(spec).map_err(|e| e.to_string())?;
    let p = build_project(&s).map_err(|e| e.to_string())?;
    let g = check_project(&s, &p);
    if !g.passed() {
        return Err(g.failures.join("; "));
    }
    Ok(p)
}

/// Runs the loop from `spec`; the first build must succeed, later failures end the loop with
/// the best round kept.
pub fn refine(spec: J, r: &mut dyn Renderer, c: &mut dyn Critic, cfg: LoopCfg) -> Result<Refined, String> {
    let mut cur = spec;
    let mut p = build(&cur)?;
    let mut rounds = vec![];
    let (mut best, mut best_score) = (cur.clone(), f64::MIN);
    for _ in 0..cfg.max_rounds.max(1) {
        let shots = r.render(&p)?;
        let cr = c.critique(&cur, &facts(&p), &shots)?;
        let score = cr.total();
        if score > best_score {
            best = cur.clone();
            best_score = score;
        }
        let done = score >= cfg.target || cr.patch.as_object().is_none_or(|o| o.is_empty());
        let patch = cr.patch.clone();
        rounds.push(Round {
            spec: cur.clone(),
            score,
            critique: Some(cr),
            error: None,
        });
        if done {
            break;
        }
        let mut next = cur.clone();
        merge_patch(&mut next, &patch);
        match build(&next) {
            Ok(np) => {
                cur = next;
                p = np;
            }
            Err(e) => {
                rounds.push(Round {
                    spec: next,
                    score: 0.0,
                    critique: None,
                    error: Some(e),
                });
                break;
            }
        }
    }
    Ok(Refined {
        best,
        score: best_score,
        rounds,
    })
}

/// An offline critic from the spec alone: the design fields a contemporary result needs.
#[derive(Default)]
pub struct RuleCritic;

impl Critic for RuleCritic {
    fn critique(&mut self, spec: &J, _facts: &J, _shots: &[Snapshot]) -> Result<Critique, String> {
        let d = parse_spec(spec).map(|s| s.design).unwrap_or_else(|_| Design::default());
        let has = |k: &str| !spec["design"][k].is_null();
        let mut fixes = vec![];
        // a field the person set, in the design or as a rule, is theirs: never overridden
        let mut fix = |rule: &str, value: J, key: &'static str| {
            let alias = super::rules::def(rule).and_then(|d| d.alias.clone()).unwrap_or_default();
            let field = alias.strip_prefix("design.").unwrap_or("");
            if (field.is_empty() || spec["design"][field].is_null()) && spec["rules"][rule].is_null() {
                fixes.extend(RuleFix::keyed(rule, value, key, vec![]));
            }
        };
        let mut scores = vec![];
        let massing = if d.massing != Massing::Bar || d.setback > 0.0 || d.cantilever > 0.0 { 9.0 } else { 6.0 };
        scores.push(("massing".to_string(), massing));
        let facade = if d.facade == FacadeSystem::Punched { 5.0 } else { 9.0 };
        if facade < 8.0 {
            fix("facade.system", json!("fins"), "fix_facade_rhythm");
        }
        scores.push(("facade".to_string(), facade));
        let entrance = if d.canopy { 9.0 } else { 5.0 };
        if !d.canopy {
            fix("site.canopy", json!(true), "fix_canopy");
        }
        scores.push(("entrance".to_string(), entrance));
        let site = if d.trees > 0 && d.paving > 0.0 { 9.0 } else { 5.0 };
        if !has("trees") && d.trees == 0 {
            fix("site.trees", json!(6), "fix_bare_site");
        }
        scores.push(("site".to_string(), site));
        scores.push(("brief".to_string(), 9.0));
        Ok(Critique {
            scores,
            patch: spec_patch(&fixes),
            notes: String::new(),
            rules: fixes,
        })
    }
}
