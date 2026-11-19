// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `generate_project` and `revise_project`: the assistant fills a building program, the
//! builder makes the model. A revision is a merge patch on the stored spec, rebuilt in place.

use super::exec;
use super::fmt::{self, Out};
use super::plan::{self, PlanOp};
use super::project_plan::{self, ProjectOp};
use super::session::{Core, Done, Handler};
use super::tools_write::why_of;
use ddc_ai::registry::{CmdError, ErrorCode};
use ddc_ai::CallCx;
use ddc_gen::project::{block, critique, slots};
use serde_json::{json, Value as J};
use std::collections::BTreeMap;
use web_time::Instant;

pub const HANDLERS: [(&str, Handler); 3] = [
    ("generate_project", generate_project),
    ("revise_project", revise_project),
    ("generate_block", generate_block),
];

/// What deepening this draft to the project depth setting costs, from its task graph sizes
/// (the gate counts); no model call.
fn depth_estimate(c: &Core, b: &project_plan::Built) -> J {
    let s = c.ed.model.depth_settings().unwrap_or_default();
    let e = ddc_model::depth::estimate(&b.gates.counts, &s);
    json!({
        "setting": crate::depth_ui::settings_json(&s),
        "to_reach_setting": crate::depth_ui::estimate_json(&e),
    })
}

fn ms(t: Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

/// Elements an item created on the scratch copy, by table.
fn created(c: &Core, item: u32) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for (id, steps) in &c.plan.steps {
        if *id != item {
            continue;
        }
        for s in steps {
            for k in exec::created_keys(s) {
                *out.entry(k.0).or_default() += 1;
            }
        }
    }
    out
}

/// Project ids of the drafted items.
fn drafted(c: &Core) -> Vec<String> {
    c.plan
        .items
        .iter()
        .filter_map(|i| match &i.op {
            PlanOp::Project(p) => Some(p.pid.clone()),
            _ => None,
        })
        .collect()
}

/// Buildings already on the site: drafted projects (the latest item of each) and those an
/// earlier Apply stored, except `skip`. Each is built once and kept for the next call.
fn site_projects(c: &Core, skip: &[&str]) -> Vec<(String, ddc_gen::project::GProject)> {
    let mut inputs: Vec<(String, J)> = c
        .plan
        .items
        .iter()
        .rev()
        .filter_map(|i| match &i.op {
            PlanOp::Project(p) => Some((p.pid.clone(), p.input.clone())),
            _ => None,
        })
        .collect();
    for (pid, head) in project_plan::stored(&c.ed.model).unwrap_or_default() {
        inputs.push((pid, head["input"].clone()));
    }
    let mut seen: Vec<String> = skip.iter().map(|s| s.to_string()).collect();
    let mut out = vec![];
    for (pid, input) in inputs {
        if seen.contains(&pid) {
            continue;
        }
        seen.push(pid.clone());
        let r = project_plan::with_model_terrain(&c.ed.model, &input);
        if let Ok(b) = project_plan::build(&r) {
            project_plan::prime(&r, b.clone());
            out.push((pid, b.project));
        }
    }
    out
}

/// A new or changed building `pid` must keep clear of every other project on the site by the
/// block rules of its brief; refused with the distances, the rule and a free origin otherwise.
fn clear_of_site(c: &Core, pid: &str, p: &ddc_gen::project::GProject, input: &J) -> Result<(), String> {
    let mut bad = vec![];
    let (mut east, mut need) = (f64::MIN, 0.0_f64);
    for (other, q) in site_projects(c, &[pid]) {
        let k = block::clearance(p, &q, &input["rules"])?;
        if let Some((_, hi)) = block::extent(&q) {
            east = east.max(hi.0);
        }
        need = need.max(k.need);
        if k.overlaps() {
            bad.push(format!("it overlaps {other}"));
        } else if !k.holds() {
            bad.push(format!("it stands {:.2} m from {other}, {} asks {:.2} m", k.gap, k.rule, k.need));
        }
    }
    if bad.is_empty() {
        return Ok(());
    }
    let free = block::extent(p).map_or(String::new(), |(lo, _)| {
        let (ox, oy) = (input["origin"][0].as_f64().unwrap_or(0.0), input["origin"][1].as_f64().unwrap_or(0.0));
        format!(" (east of every project: origin [{:.1}, {oy:.1}])", east + need - (lo.0 - ox))
    });
    Err(format!(
        "{pid} is not drafted: {}. Give it an origin [x, y] clear of the others{free}, or draft the buildings together with generate_block; rules.block.spacing and block.sun_ratio in the spec set the distance",
        bad.join("; ")
    ))
}

/// Elements an item left as they are, with the reason: hand edits since the last Apply.
fn skipped(c: &Core, item: u32) -> Vec<J> {
    c.plan
        .skips
        .get(&item)
        .map(|v| {
            v.iter()
                .take(20)
                .map(|s| json!({"element": format!("{} {}", s.cat, s.id), "why": s.why}))
                .collect()
        })
        .unwrap_or_default()
}

/// A project stored by an earlier Apply: a new item brings the model to the patched spec.
fn revise_applied(c: &mut Core, a: &J, cx: &CallCx<'_>, pid: Option<&str>) -> Result<Done, String> {
    let t = Instant::now();
    let stored = project_plan::stored(&c.ed.model).map_err(|e| e.to_string())?;
    let found = match pid {
        Some(p) => stored.iter().find(|s| s.0 == p),
        None => stored.last(),
    };
    let Some((pid, head)) = found.cloned() else {
        return Err(match pid {
            Some(p) => format!("no project {p} in this plan or the model"),
            None => "no project in this plan or the model: call generate_project".to_string(),
        });
    };
    let level = head["level"]
        .as_i64()
        .ok_or("the stored project has no level")?;
    if !c.ed.levels().iter().any(|l| l.0 == level) {
        return Err(format!(
            "the ground level of {pid} was deleted: call generate_project instead"
        ));
    }
    let (input, refused) = project_plan::revise(&c.ed.model, &head["input"], &a["changes"])?;
    let b = build_brief(&c.ed.model, &input)?;
    clear_of_site(c, &pid, &b.project, &input)?;
    let item = c.next_item_id();
    let slot0 = c.plan.reserve_slots(slots::TOTAL, item);
    let title = match a["title"].as_str() {
        Some(s) if !s.trim().is_empty() => fmt::text(s),
        _ => format!("{} ({pid})", b.spec.name),
    };
    let op = PlanOp::Project(ProjectOp {
        input,
        level,
        slot0,
        pid: pid.clone(),
    });
    let added = c.add_item(title, why_of(a), op, cx.actor)?;
    let fields: Vec<&String> = a["changes"]
        .as_object()
        .map(|p| p.keys().collect())
        .unwrap_or_default();
    let mut o = Out::new(c.max_chars);
    o.put("item", &json!(added.0));
    o.put("project_id", &json!(pid));
    o.put("changed_fields", &json!(fields));
    o.put(
        "changes",
        &json!({"created": added.1 .0, "changed": added.1 .1, "deleted": added.1 .2}),
    );
    let kept = skipped(c, added.0);
    if !kept.is_empty() {
        o.put("kept_hand_edits", &J::from(kept));
    }
    if !refused.is_empty() {
        o.put("refused_by_locks", &J::from(refused));
    }
    o.put_fit("project", &project_plan::summary(&b), SHED);
    o.put("depth_estimate", &depth_estimate(c, &b));
    o.put("ms", &json!(ms(t)));
    if !super::mep_gate::is_on_now(&c.ed.model) {
        o.put("notice", &super::mep_gate::notice());
    }
    o.put("note", &json!(ddc_gen::SCHEMATIC_NOTE));
    Ok(o.finish().into())
}

/// Project setting: run the offline design critique on a brief before it is drafted. On by
/// default; `off` drafts every brief as given (the harness A/B sets it either way).
pub const POLICY_REFINE: &str = "ai.gen_refine";

/// What a long project summary sheds first, so `why` and `brief_check` still arrive.
const SHED: &[&str] = &["more_warnings", "warnings", "score", "metrics", "gates", "counts"];

/// The rule critic's loop, as many rounds as the brief's `layout.refine_rounds` allows (none
/// at 0): no model call, no images, never a field the person set. Reads the model's terrain.
fn refine(m: &ddc_model::Model, spec: &J) -> Result<Option<critique::Refined>, String> {
    let read = project_plan::with_model_terrain(m, spec);
    let cfg = critique::LoopCfg::of(&read)?;
    if cfg.max_rounds == 0 {
        return Ok(None);
    }
    let mut r = critique::refine(read, &mut critique::NoShots, &mut critique::RuleCritic, cfg)
        .map_err(|e| format!("the brief does not build: {e}"))?;
    project_plan::keep_model_terrain(spec, &mut r.best);
    Ok(Some(r))
}

/// What the reply says about the critique: rounds, the score and the fields it changed.
fn refine_report(r: &critique::Refined) -> J {
    let rounds: Vec<J> = r
        .rounds
        .iter()
        .map(|x| match &x.critique {
            Some(c) => json!({"score": (x.score * 10.0).round() / 10.0, "patch": c.patch,
                              "rules": c.rules.iter().map(|f| f.to_json()).collect::<Vec<_>>()}),
            None => json!({"error": x.error}),
        })
        .collect();
    json!({"score": (r.score * 10.0).round() / 10.0, "rounds": rounds,
           "note": "design fields filled by the offline critique; revise_project changes them back"})
}

/// A brief that does not build is the brief's fault. Uncoded, its "why not" (the conflicting
/// demands) read as a plan conflict and its gate failures as an editor fault: neither is
/// something the assistant can fix, so it retried the same call (seen live: 70 refusals).
fn build_brief(m: &ddc_model::Model, input: &J) -> Result<project_plan::Built, String> {
    project_plan::build_on(m, input).map_err(|e| brief_error(&e))
}

pub(super) fn brief_error(e: &str) -> String {
    match CmdError::from_text(e).code {
        ErrorCode::InvalidArgs | ErrorCode::NotFound | ErrorCode::Limit => e.to_string(),
        _ => CmdError::wire(ErrorCode::InvalidArgs, e),
    }
}

pub(super) fn generate_project(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let t = Instant::now();
    let mut input = super::norm_tools::with_pack_norms(c, a["pack"].as_str(), a["spec"].clone())?;
    let refined = match super::ui::connect::policy_flag(&c.ed.model, POLICY_REFINE, true) {
        // a brief the critic cannot build is refused by the plain build below, with its why not
        true => refine(&c.ed.model, &input).ok().flatten().map(|r| {
            input = r.best.clone();
            refine_report(&r)
        }),
        false => None,
    };
    let b = build_brief(&c.ed.model, &input)?;
    let pid = project_plan::next_pid(&c.ed.model, &drafted(c)).map_err(|e| e.to_string())?;
    clear_of_site(c, &pid, &b.project, &input)?;
    project_plan::prime(&project_plan::with_model_terrain(&c.ed.model, &input), b.clone());
    let built = ms(t);
    let item = c.next_item_id();
    let slot0 = c.plan.reserve_slots(slots::TOTAL, item);
    let title = match a["title"].as_str() {
        Some(s) if !s.trim().is_empty() => fmt::text(s),
        _ => b.spec.name.clone(),
    };
    let op = PlanOp::Project(ProjectOp {
        input,
        level: c.view.level,
        slot0,
        pid: pid.clone(),
    });
    let added = c.add_item(title, why_of(a), op, cx.actor)?;
    let total = ms(t);
    let mut o = Out::new(c.max_chars);
    o.put("item", &json!(added.0));
    o.put("project_id", &json!(pid));
    o.put(
        "changes",
        &json!({"created": added.1 .0, "changed": added.1 .1, "deleted": added.1 .2}),
    );
    if let Some(r) = &refined {
        o.put("refined", r);
    }
    o.put_fit("project", &project_plan::summary(&b), SHED);
    o.put("depth_estimate", &depth_estimate(c, &b));
    o.put("created", &json!(created(c, added.0)));
    o.put(
        "references",
        &json!(format!(
            "floor k (from 0) below {} less the basements uses slots {slot0} + k*{}: walls +{}, openings +{}, rooms +{}; higher floors live in later blocks and keep them when floors are added",
            slots::LOW_STOREYS,
            slots::PER_STOREY,
            slots::WALLS,
            slots::OPENINGS,
            slots::ROOMS
        )),
    );
    o.put("ms", &json!({"build": built, "draft": total}));
    o.put(
        "revise",
        &json!("revise_project with a merge patch keeps the references, also after Apply"),
    );
    if !super::mep_gate::is_on_now(&c.ed.model) {
        o.put("notice", &super::mep_gate::notice());
    }
    o.put("note", &json!(ddc_gen::SCHEMATIC_NOTE));
    Ok(o.finish().into())
}

pub(super) fn revise_project(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let t = Instant::now();
    let patch = &a["changes"];
    if !patch.is_object() {
        return Err("changes must be an object: the spec fields that change".to_string());
    }
    let pid = a["project"].as_str();
    let is_project = |op: &PlanOp| match op {
        PlanOp::Project(p) => pid.is_none_or(|x| x == p.pid),
        _ => false,
    };
    let pos = match a["item"].as_u64() {
        Some(id) => Some(
            c.plan
                .items
                .iter()
                .position(|i| u64::from(i.id) == id && is_project(&i.op))
                .ok_or_else(|| format!("item {id} is not a drafted project"))?,
        ),
        None => c.plan.items.iter().rposition(|i| is_project(&i.op)),
    };
    let Some(pos) = pos else {
        return revise_applied(c, a, cx, pid);
    };
    let id = c.plan.items[pos].id;
    let PlanOp::Project(op) = &c.plan.items[pos].op else {
        return Err("not a project".to_string());
    };
    let old = op.input.clone();
    let own = op.pid.clone();
    let (input, refused) = project_plan::revise(&c.ed.model, &old, patch)?;
    // refused here, the plan is untouched
    let b = build_brief(&c.ed.model, &input)?;
    clear_of_site(c, &own, &b.project, &input)?;
    let before = created(c, id);
    if let PlanOp::Project(op) = &mut c.plan.items[pos].op {
        op.input = input;
    }
    c.plan.items[pos].checked = true;
    if let Err(e) = c.replan() {
        if let PlanOp::Project(op) = &mut c.plan.items[pos].op {
            op.input = old;
        }
        let back = c.replan().err().map(|x| x.to_string());
        return Err(format!(
            "the revised project did not compile ({e}); the previous spec is back{}",
            back.map(|x| format!(", but the replay failed: {x}"))
                .unwrap_or_default()
        ));
    }
    let after = created(c, id);
    let fields: Vec<&String> = patch
        .as_object()
        .map(|p| p.keys().collect())
        .unwrap_or_default();
    let title = fmt::text(a["title"].as_str().unwrap_or("revise project"));
    c.audit_as(
        cx.actor,
        "tool_write",
        Some(format!("{id}: revised {}", fields.len())),
        plan::audit_text(&title, &why_of(a)),
        "ok",
    );
    let mut o = Out::new(c.max_chars);
    o.put("item", &json!(id));
    o.put("changed_fields", &json!(fields));
    o.put("created_before", &json!(before));
    o.put("created_after", &json!(after));
    let kept = skipped(c, id);
    if !kept.is_empty() {
        o.put("kept_hand_edits", &J::from(kept));
    }
    if !refused.is_empty() {
        o.put("refused_by_locks", &J::from(refused));
    }
    o.put_fit("project", &project_plan::summary(&b), SHED);
    o.put("depth_estimate", &depth_estimate(c, &b));
    o.put("ms", &json!(ms(t)));
    if !super::mep_gate::is_on_now(&c.ed.model) {
        o.put("notice", &super::mep_gate::notice());
    }
    o.put("note", &json!(ddc_gen::SCHEMATIC_NOTE));
    Ok(o.finish().into())
}

/// Items of a block taken back: the plan is as it was before the call.
fn drop_items(c: &mut Core, ids: &[u32]) -> Option<String> {
    c.plan.items.retain(|i| !ids.contains(&i.id));
    c.plan.slot_owner.retain(|_, owner| !ids.contains(owner));
    c.replan().err().map(|e| e.to_string())
}

/// A block of buildings on plots: one rule-driven brief per plot, built in parallel, checked as a
/// block, then one project item per building. All or none: a plot that does not build or a
/// broken block rule refuses the whole call and leaves the plan as it was.
pub(super) fn generate_block(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let t = Instant::now();
    let mut args = a.clone();
    let base = if a["spec"].is_null() { json!({}) } else { a["spec"].clone() };
    args["spec"] = super::norm_tools::with_pack_norms(c, a["pack"].as_str(), base)?;
    let input = block::read(&args)?;
    let plan = block::plan(&input)?;
    let t_plan = ms(t);
    let t1 = Instant::now();
    let resolved: Vec<J> = plan
        .plots
        .iter()
        .map(|p| project_plan::with_model_terrain(&c.ed.model, &p.brief))
        .collect();
    let workers = block::workers();
    let mut built = Vec::with_capacity(resolved.len());
    let mut errs = vec![];
    for (p, r) in plan.plots.iter().zip(block::build_all(&resolved, workers)) {
        match r {
            Ok(b) => built.push(b),
            Err(e) => errs.push(format!("plot {}: {e}", p.id)),
        }
    }
    if !errs.is_empty() {
        return Err(format!("the block does not build, nothing was drafted:\n{}", errs.join("\n")));
    }
    let t_build = ms(t1);
    let t2 = Instant::now();
    let report = block::check(&plan, &built.iter().map(|b| &b.project).collect::<Vec<_>>());
    if !report.failures.is_empty() {
        return Err(format!(
            "the block breaks its rules, nothing was drafted: {}",
            report.failures.join("; ")
        ));
    }
    // the block against what already stands on the site
    for (p, b) in plan.plots.iter().zip(&built) {
        clear_of_site(c, &format!("plot {}", p.id), &b.project, &p.brief)
            .map_err(|e| format!("the block is too close to other projects, nothing was drafted: {e}"))?;
    }
    let t_check = ms(t2);
    let t3 = Instant::now();
    let mut done: Vec<(u32, String, (usize, usize, usize))> = vec![];
    let label = match a["title"].as_str() {
        Some(s) if !s.trim().is_empty() => fmt::text(s),
        _ => "Block".to_string(),
    };
    for ((p, b), r) in plan.plots.iter().zip(built).zip(&resolved) {
        project_plan::prime(r, b.into());
        let item = c.next_item_id();
        let slot0 = c.plan.reserve_slots(slots::TOTAL, item);
        let pid = project_plan::next_pid(&c.ed.model, &drafted(c)).map_err(|e| e.to_string())?;
        let title = format!("{label} {}: {} {} storeys", p.id, p.program, p.floors);
        let op = PlanOp::Project(ProjectOp {
            input: p.brief.clone(),
            level: c.view.level,
            slot0,
            pid: pid.clone(),
        });
        match c.add_item(title, why_of(a), op, cx.actor) {
            Ok(added) => done.push((added.0, pid, added.1)),
            Err(e) => {
                let ids: Vec<u32> = done.iter().map(|d| d.0).chain([item]).collect();
                let back = drop_items(c, &ids).map(|x| format!("; taking the block back failed: {x}"));
                return Err(format!(
                    "plot {}: {e}; nothing of the block was drafted{}",
                    p.id,
                    back.unwrap_or_default()
                ));
            }
        }
    }
    let t_draft = ms(t3);
    let summary = report.to_json();
    let facts: Vec<J> = summary["buildings"].as_array().cloned().unwrap_or_default();
    let rows: Vec<J> = plan
        .plots
        .iter()
        .zip(&done)
        .zip(facts)
        .map(|((p, d), f)| {
            json!({"plot": p.id, "item": d.0, "project_id": d.1, "program": p.program,
                   "floors": p.floors, "created": d.2 .0, "facts": f, "why": p.why})
        })
        .collect();
    let mut o = Out::new(c.max_chars);
    o.put("buildings", &json!(rows.len()));
    o.put("items", &json!(done.iter().map(|d| d.0).collect::<Vec<_>>()));
    o.put(
        "block",
        &json!({"passed": true, "min_gap_m": summary["min_gap_m"],
                "rules": {"block.spacing": plan.spacing, "block.sun_ratio": plan.sun_ratio,
                          "block.fire_access": plan.fire_access}}),
    );
    o.put(
        "ms",
        &json!({"plan": t_plan, "build_parallel": t_build, "check": t_check, "draft": t_draft,
                "total": ms(t), "workers": workers.clamp(1, plan.plots.len().max(1))}),
    );
    o.list("per_plot", &rows);
    o.put(
        "revise",
        &json!("revise_project with project P<n> changes one building; drop_item takes one back"),
    );
    if !super::mep_gate::is_on_now(&c.ed.model) {
        o.put("notice", &super::mep_gate::notice());
    }
    o.put("note", &json!(ddc_gen::SCHEMATIC_NOTE));
    Ok(o.finish().into())
}
