// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Chat tools over the generation rules (group `gen`): list and read rules, explain, conflicts
//! and variants; change rules or locks, or regenerate. The model writes rule patches and locks
//! only, never geometry. Every change is a revision of the project, drafted for the person's
//! review and applied in one undo step, like `revise_project`.

use super::fmt::Out;
use super::gen_rules as gr;
use super::plan::PlanOp;
use super::project_plan::{keep_model_terrain, with_model_terrain};
use super::project_tools;
use super::qspatial::parse_ref;
use super::session::{Core, Done, Handler};
use ddc_ai::CallCx;
use ddc_gen::project::{merge_patch, rules, variants};
use serde_json::{json, Value as J};

pub const HANDLERS: [(&str, Handler); 9] = [
    ("list_rules", list_rules),
    ("get_rule", get_rule),
    ("set_rule", set_rule),
    ("lock", lock),
    ("unlock", unlock),
    ("explain", explain),
    ("conflicts", conflicts),
    ("compare_variants", compare_variants),
    ("regenerate", regenerate),
];

/// The project a tool works on: a drafted item, else a project stored by Apply.
pub struct Current {
    pub input: J,
    /// What the generator reads: `input` with `site.terrain: "model"` as the model's points.
    pub read: J,
    pub item: Option<u32>,
    pub pid: String,
}

impl Current {
    fn new(c: &Core, input: J, item: Option<u32>, pid: String) -> Current {
        let read = with_model_terrain(&c.ed.model, &input);
        Current { input, read, item, pid }
    }
}

pub fn current(c: &Core, pid: Option<&str>) -> Result<Current, String> {
    let drafted = c.plan.items.iter().rev().find_map(|i| match &i.op {
        PlanOp::Project(p) if pid.is_none_or(|x| x == p.pid) => Some((i.id, p)),
        _ => None,
    });
    if let Some((id, p)) = drafted {
        return Ok(Current::new(c, p.input.clone(), Some(id), p.pid.clone()));
    }
    let stored = super::project_plan::stored(&c.ed.model).map_err(|e| e.to_string())?;
    let found = match pid {
        Some(p) => stored.into_iter().find(|s| s.0 == p),
        None => stored.into_iter().last(),
    };
    match found {
        Some((pid, head)) => Ok(Current::new(c, head["input"].clone(), None, pid)),
        None => Err(match pid {
            Some(p) => format!("no project {p} in this plan or the model"),
            None => "no generated project in this plan or the model: call generate_project".into(),
        }),
    }
}

fn of(c: &Core, a: &J) -> Result<Current, String> {
    current(c, a["project"].as_str())
}

fn reply(c: &Core, why: &str, fill: impl FnOnce(&mut Out)) -> Result<Done, String> {
    let mut o = Out::new(c.max_chars);
    o.put_own("why", &json!(why));
    fill(&mut o);
    Ok(o.finish().into())
}

/// A change a tool drafts: the next brief, what was asked, and why it is safe.
struct Change {
    next: J,
    asked: J,
    why: &'static str,
    /// A brief that cannot build is refused with its conflict; otherwise the change is applied
    /// around the locks, piece by piece.
    strict: bool,
}

/// Drafts the change as a revision of the project.
fn revise(c: &mut Core, a: &J, cx: &CallCx<'_>, cur: &Current, ch: Change) -> Result<Done, String> {
    let Change { next, asked, why, strict } = ch;
    let read = with_model_terrain(&c.ed.model, &next);
    if strict {
        if let Some(k) = gr::conflicts(&read, None) {
            let fixes: Vec<J> = k.fixes.iter().map(gr::fix_json).collect();
            let v = json!({"error": k.error, "conflict": k.labels, "relax": fixes,
                           "why": "nothing changed: dropping any one member of the conflict lets the project build; each relax alone does too and is a set_rule patch"});
            return Err(v.to_string());
        }
    }
    let (mut built, locks) = gr::around_locks(&cur.read, &read)?;
    keep_model_terrain(&next, &mut built);
    let next = built;
    let changes = gr::diff_patch(&cur.input, &next);
    if changes.as_object().is_none_or(|o| o.is_empty()) {
        return Err(format!("nothing changes for {}; locks: {locks}", cur.pid));
    }
    let mut args = json!({"changes": changes, "project": cur.pid});
    if let Some(i) = cur.item {
        args["item"] = json!(i);
    }
    for k in ["title", "why"] {
        if let Some(x) = a.get(k).filter(|x| !x.is_null()) {
            args[k] = x.clone();
        }
    }
    let done = project_tools::revise_project(c, &args, cx)?;
    let mut rev: J = serde_json::from_str(&done.reply.text).unwrap_or(J::Null);
    if let Some(o) = rev.as_object_mut() {
        o.remove("untrusted");
    }
    reply(c, why, |o| {
        o.put("asked", &asked);
        o.put_own("locks", &locks);
        o.put_own("revision", &rev);
        o.put_own("apply", &json!("a draft: the person reviews it and applies it in one undo step"));
    })
}

fn list_rules(c: &mut Core, a: &J, _: &CallCx<'_>) -> Result<Done, String> {
    let cur = of(c, a)?;
    let b = gr::build(&cur.read)?;
    let group = a["group"].as_str();
    let groups = rules::groups();
    if let Some(g) = group.filter(|g| !groups.contains(g)) {
        return Err(format!("no group {g}; groups: {}", groups.join(", ")));
    }
    let q = a["q"].as_str().unwrap_or("");
    let changed = a["changed"].as_bool().unwrap_or(false);
    let rows = gr::rows(&cur.input, &b.spec, Some(&b.score));
    let narrow = group.is_some() || !q.trim().is_empty() || changed;
    let hits: Vec<&gr::RuleRow> = if narrow {
        gr::filter(&rows, group, q, changed)
    } else {
        // the overview: what limits the score, what was changed, what is locked
        rows.iter().filter(|r| r.limiting() || r.locked || !matches!(r.source, "builtin" | "typology")).collect()
    };
    let counts: Vec<J> = groups
        .iter()
        .map(|g| json!([g, rows.iter().filter(|r| r.def.group == *g).count()]))
        .collect();
    let items: Vec<J> = hits.iter().map(|r| r.to_json()).collect();
    let why = "values in force: built-in set, then norm, preset, project and brief layers (source); a goal's tier \
               dominates every weaker tier, its weight counts within the tier; cost > 0 marks a goal that limits \
               the score now; a locked rule refuses set_rule until unlock";
    reply(c, why, |o| {
        o.put("project", &json!(cur.pid));
        if !narrow {
            o.put_own("groups", &J::from(counts));
        }
        o.put("shown", &json!(items.len()));
        o.list("rules", &items);
    })
}

fn get_rule(c: &mut Core, a: &J, _: &CallCx<'_>) -> Result<Done, String> {
    let id = a["id"].as_str().ok_or("id is a rule id such as grid.span")?;
    let d = rules::def(id).ok_or(format!("no rule {id}: list_rules shows them"))?;
    let cur = of(c, a)?;
    let b = gr::build(&cur.read)?;
    let row =gr::rows(&cur.input, &b.spec, Some(&b.score)).into_iter().find(|r| r.def.id == id);
    let drives: Vec<J> = gr::explain(&b.spec, &b.project, &[id.to_string()]).iter().map(|w| w.to_json()).collect();
    let goal = b.score.costs.iter().find(|x| x.rule == id).map(|x| x.to_json());
    let why = "the decisions this rule drives in the current build, each with its measured effect; set_rule \
               changes it within its range";
    reply(c, why, |o| {
        o.put("project", &json!(cur.pid));
        o.put("rule", &row.map_or(J::Null, |r| r.to_json()));
        o.put_own("group", &json!(d.group));
        o.put_own("unit", &json!(d.unit));
        o.put_own("range", &json!([d.min, d.max]));
        o.put_own("default", &d.default.map_or(J::Null, |x| d.value_json(x)));
        if let Some(g) = &goal {
            o.put_own("goal", g);
        }
        o.list("drives", &drives);
    })
}

fn set_rule(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let cur = of(c, a)?;
    let next = gr::set_rules(&cur.input, &a["rules"])
        .map_err(|e| format!("refused, nothing changed: {}", e.join("; ")))?;
    let why = "every value was checked against its rule's range; the project is rebuilt from the rules around \
               its locks";
    revise(c, a, cx, &cur, Change { next, asked: a["rules"].clone(), why, strict: true })
}

fn targets(a: &J) -> Vec<J> {
    a["targets"].as_array().cloned().unwrap_or_default()
}

fn lock(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let cur = of(c, a)?;
    let b = gr::build(&cur.read)?;
    let (next, labels) = gr::add_locks(&cur.input, &b, &targets(a))?;
    let why = "sealed from the current build: any later change that would alter a locked part is refused and \
               reported, never applied silently";
    revise(c, a, cx, &cur, Change { next, asked: json!(labels), why, strict: true })
}

fn unlock(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let cur = of(c, a)?;
    let all = a["all"].as_bool().unwrap_or(false);
    let (next, labels) = gr::remove_locks(&cur.input, &targets(a), all)?;
    let why = "released: these parts follow the rules again on the next change";
    revise(c, a, cx, &cur, Change { next, asked: json!(labels), why, strict: true })
}

fn explain(c: &mut Core, a: &J, _: &CallCx<'_>) -> Result<Done, String> {
    let (cur, refs) = match a["element"].as_str() {
        Some(e) => {
            let key = if e == "selection" {
                let keys = c.selection_keys();
                match keys.as_slice() {
                    [k] => (k.cat.to_string(), k.id),
                    _ => return Err(format!("select one element, not {}", keys.len())),
                }
            } else {
                let (cat, id) = parse_ref(e)?;
                (cat.to_string(), id)
            };
            let (pid, _, refs) = gr::element_refs(&c.ed.model, &key.0, key.1)?;
            (current(c, Some(pid.as_str()))?, refs)
        }
        None => (of(c, a)?, vec![a["ref"].as_str().unwrap_or("score").to_string()]),
    };
    let b = gr::build(&cur.read)?;
    let reasons: Vec<J> = gr::explain(&b.spec, &b.project, &refs).iter().map(|w| w.to_json()).collect();
    if reasons.is_empty() {
        return Err(format!(
            "nothing explains {}: use a part (massing, grid, core, facade, rooms, storey:N, basement, site), a rule id or score",
            refs.join(", ")
        ));
    }
    let why = "each reason names the rule, its value, the layer it came from and its measured effect on this \
               build; set_rule or lock steers it";
    reply(c, why, |o| {
        o.put("project", &json!(cur.pid));
        o.put("refs", &json!(refs));
        o.list("reasons", &reasons);
    })
}

fn conflicts(c: &mut Core, a: &J, _: &CallCx<'_>) -> Result<Done, String> {
    let cur = of(c, a)?;
    let changes = a.get("changes").filter(|x| x.is_object());
    match gr::conflicts(&cur.read, changes) {
        None => reply(c, "it builds, passes its gates, meets every hard goal and keeps its locks: no conflict", |o| {
            o.put("project", &json!(cur.pid));
            o.put("feasible", &json!(true));
        }),
        Some(k) => {
            let fixes: Vec<J> = k.fixes.iter().map(gr::fix_json).collect();
            let why = "the smallest set of demands that cannot hold together: dropping any one lets it build; each \
                       relax alone resolves it and is a ready set_rule patch";
            reply(c, why, |o| {
                o.put("project", &json!(cur.pid));
                o.put("feasible", &json!(false));
                o.put("error", &json!(k.error));
                o.put("conflict", &json!(k.labels));
                o.list("relax", &fixes);
            })
        }
    }
}

fn vary_of(a: &J) -> Vec<String> {
    a["vary"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect()
}

fn found(a: &J, cur: &Current, n: usize) -> Result<variants::Variants, String> {
    let seed = a["seed"].as_u64().unwrap_or(0);
    let cfg = gr::vary_cfg(&cur.read, n, seed, &vary_of(a))?;
    gr::variants(&cur.read, &cfg)
}

fn compare_variants(c: &mut Core, a: &J, _: &CallCx<'_>) -> Result<Done, String> {
    let cur = of(c, a)?;
    let n = a["n"].as_u64().unwrap_or(3) as usize;
    let v = found(a, &cur, n)?;
    let cmp = variants::compare(&v);
    let why = "variants differ on purpose (pairwise distance at least min_distance) and rank tier by tier; \
               why_not_best names the goals that cost more than the best; nothing changed: regenerate with \
               variant, the same seed and vary drafts one";
    reply(c, why, |o| {
        o.put("project", &json!(cur.pid));
        o.put("seed", &json!(a["seed"].as_u64().unwrap_or(0)));
        o.put_own("compare", &cmp);
    })
}

fn regenerate(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let cur = of(c, a)?;
    let (next, asked) = if let Some(k) = a["variant"].as_u64() {
        let v = found(a, &cur, gr::MAX_VARIANTS)?;
        let x = v
            .chosen
            .get((k as usize).saturating_sub(1))
            .ok_or(format!("variant {k} of {}: call compare_variants with the same seed and vary", v.chosen.len()))?;
        let set: serde_json::Map<String, J> = x.set.iter().cloned().collect();
        let mut brief = x.brief.clone();
        keep_model_terrain(&cur.input, &mut brief);
        (brief, json!({"variant": k, "rules": set}))
    } else if a["changes"].is_object() {
        let mut next = cur.input.clone();
        merge_patch(&mut next, &a["changes"]);
        (next, a["changes"].clone())
    } else {
        return Err("give changes (a spec merge patch) or variant (from compare_variants)".into());
    };
    let why = "rebuilt around the locks: changes a lock forbids are refused one by one and listed under \
               locks.refused, the rest applied";
    revise(c, a, cx, &cur, Change { next, asked, why, strict: false })
}
