// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Runs cases without a window: an in-memory project, a session in propose mode, the plan
//! applied the way the person's Apply does, then the oracles on the result.

use super::case::{evals_dir, Case};
use super::facts::Facts;
use super::metrics::{CaseMetrics, Status};
use super::oracle::{blocking_findings, evaluate, EvalCx};
use super::replay;
use crate::ai::receipt::WriteReceipt;
use crate::ai::session::{UiEvent, CONFIRM_TTL};
use crate::ai::validate::{IssueLevel, PlanIssue};
use crate::ai::{SessionCfg, UiSession, ViewCtx};
use crate::editor::Editor;
use ddc_ai::fake::{RecordingProvider, ReplayProvider};
use ddc_ai::{build_provider, AuthKind, EgressGuard, Limits, Mode, Profile, Protocol, Provider, Secret, ToolCalling};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static SESSION: AtomicU64 = AtomicU64::new(0);

/// Settings that do not depend on the case.
#[derive(Clone)]
pub struct Opts {
    /// Where finished projects are saved; `@id` bases are read from here.
    pub out: Option<PathBuf>,
    pub profile: Profile,
    /// Price per million tokens, in and out; 0 for a replay.
    pub price_in: f64,
    pub price_out: f64,
    pub turn_timeout: Duration,
}

pub fn replay_profile() -> Profile {
    ddc_ai::fake::profile()
}

/// What answers for one case.
pub struct Made {
    pub provider: Arc<dyn Provider>,
    pub replay: Option<Arc<ReplayProvider>>,
    pub rec: Option<Arc<RecordingProvider>>,
}

pub trait ProviderFactory: Sync {
    fn make(&self, case: &Case) -> Result<Made, String>;
}

/// Answers from the recordings under `root`.
pub struct ReplayFactory {
    pub root: PathBuf,
}

impl ProviderFactory for ReplayFactory {
    fn make(&self, case: &Case) -> Result<Made, String> {
        let r = replay::load(&self.root, &case.id)?;
        let rp = ReplayProvider::new(r);
        Ok(Made { provider: rp.clone(), replay: Some(rp), rec: None })
    }
}

/// The real provider; with `record` set, every answer is kept for a later replay.
pub struct LiveFactory {
    pub provider: Arc<dyn Provider>,
    pub record: bool,
}

impl ProviderFactory for LiveFactory {
    fn make(&self, _: &Case) -> Result<Made, String> {
        if self.record {
            let rec = RecordingProvider::new(self.provider.clone());
            Ok(Made { provider: rec.clone(), replay: None, rec: Some(rec) })
        } else {
            Ok(Made { provider: self.provider.clone(), replay: None, rec: None })
        }
    }
}

/// A provider from a JSON profile file (`base_url`, `auth_header`, `headers`, `model`,
/// `prices.input/output`); the key is read from `DDC_AI_KEY` here and goes nowhere else.
pub fn live_provider(profile_file: &Path) -> Result<(Profile, Arc<dyn Provider>), String> {
    let t = std::fs::read_to_string(profile_file).map_err(|e| format!("{}: {e}", profile_file.display()))?;
    let pf: serde_json::Value = serde_json::from_str(&t).map_err(|e| format!("{}: {e}", profile_file.display()))?;
    let key = std::env::var("DDC_AI_KEY").map_err(|_| "set DDC_AI_KEY".to_string())?;
    let s = |k: &str| pf[k].as_str().map(str::to_string).ok_or(format!("profile: no {k}"));
    let base_url = s("base_url")?;
    let host = base_url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("").to_string();
    let profile = Profile {
        id: "eval".into(),
        name: "eval".into(),
        protocol: Protocol::ContentBlocks,
        base_url,
        model: s("model")?,
        auth: AuthKind::Header { name: s("auth_header")? },
        extra_headers: pf["headers"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
            .collect(),
        tool_calling: ToolCalling::Native,
        images: true,
        context_tokens: 200_000,
        max_output_tokens: 8192,
        price_in_per_mtok: pf["prices"]["input"].as_f64(),
        price_out_per_mtok: pf["prices"]["output"].as_f64(),
        executor: None,
    };
    let guard = EgressGuard { local_only: false, consented_host: Some(host) };
    let provider = build_provider(&profile, Some(Secret::new(key)), guard).map_err(|e| format!("provider: {e:?}"))?;
    Ok((profile, provider))
}

fn view_of(ed: &Editor) -> ViewCtx {
    ViewCtx {
        view: "plan".to_string(),
        level: ed.level,
        units: ed.tool.units,
        selection: vec![],
        bounds: None,
        ..ViewCtx::default()
    }
}

fn op_rows(ed: &Editor) -> i64 {
    ed.model.sql().query_row("SELECT count(*) FROM op", [], |r| r.get(0)).unwrap_or(0)
}

/// Opens the case's base into `ed`; `Ok(false)` for an empty project.
fn open_base(ed: &mut Editor, case: &Case, o: &Opts) -> Result<bool, String> {
    let src = match case.base.as_str() {
        "" | "empty" => return Ok(false),
        b => match b.strip_prefix('@') {
            Some(id) => o.out.as_ref().ok_or("a base from an earlier case needs --out")?.join(format!("{id}.ddc")),
            None => evals_dir().join(b),
        },
    };
    if !src.is_file() {
        return Err(format!("base {} is missing", src.display()));
    }
    let copy = std::env::temp_dir().join(format!("ddc_eval_{}_{}.ddc", case.id, std::process::id()));
    std::fs::copy(&src, &copy).map_err(|e| format!("{}: {e}", src.display()))?;
    ed.open_project(&copy).map_err(|e| format!("{}: {e:?}", src.display()))?;
    Ok(true)
}

fn cost(o: &Opts, tin: u64, tout: u64) -> f64 {
    (tin as f64 * o.price_in + tout as f64 * o.price_out) / 1e6
}

/// One case against one provider. `cap_usd` stops the session when its cost reaches it.
pub fn run_case(case: &Case, made: &Made, o: &Opts, cap_usd: Option<f64>) -> CaseMetrics {
    let t0 = Instant::now();
    let mut m = CaseMetrics::new(&case.id);
    let fail = |mut m: CaseMetrics, status: Status, why: String, t0: Instant| {
        m.status = status;
        m.note = why;
        m.wall_s = t0.elapsed().as_secs_f64();
        m
    };
    let mut ed = match Editor::new_in_memory() {
        Ok(e) => e,
        Err(e) => return fail(m, Status::NotRun, format!("editor: {e:?}"), t0),
    };
    if let Err(e) = ed.reload() {
        return fail(m, Status::NotRun, format!("editor: {e:?}"), t0);
    }
    let has_base = match open_base(&mut ed, case, o) {
        Ok(b) => b,
        Err(e) => return fail(m, Status::NotRun, e, t0),
    };
    for (k, v) in &case.policy {
        if let Err(e) = ed.model.set_ai_policy(k, v) {
            return fail(m, Status::NotRun, format!("policy {k}: {e:?}"), t0);
        }
    }
    let before = if has_base { Facts::read(&ed.model).ok() } else { None };
    let errors_before = if has_base { blocking_findings(&ed.model).unwrap_or(0) } else { 0 };
    let ops0 = op_rows(&ed);

    let cfg = SessionCfg {
        provider: made.provider.clone(),
        profile: o.profile.clone(),
        limits: Limits {
            max_steps: 40,
            max_tool_calls: 120,
            max_turn_tokens: 8_000_000,
            max_cost: cap_usd.map(|c| c * 10.0),
            max_wall_s: 1500,
            ..Limits::default()
        },
        session: format!("eval{}", SESSION.fetch_add(1, Ordering::Relaxed)),
        allow_snapshots: Arc::new(AtomicBool::new(false)),
        extra_generators: vec![],
        extra_rules: vec![],
        keep_text: false,
        confirm_ttl: CONFIRM_TTL,
        target: ddc_ai::catalog::Target::Main,
        auto_replan: false,
    };
    let view = view_of(&ed);
    let mut s = match UiSession::start(&ed, cfg, view) {
        Ok(s) => s,
        Err(e) => return fail(m, Status::NotRun, format!("session: {e}"), t0),
    };
    if !s.wait_idle(&mut ed, o.turn_timeout) {
        return fail(m, Status::NotRun, "the session did not open".into(), t0);
    }
    let mut answer = String::new();
    let mut receipts: Vec<WriteReceipt> = vec![];
    let mut failed: Option<String> = None;
    let mut over_budget = false;
    'turns: for turn in &case.turns {
        if !answer.is_empty() {
            answer.push_str("\n\n");
        }
        let view = view_of(&ed);
        if let Err(e) = s.send_turn(&mut ed, turn.say.clone(), view, Mode::Propose) {
            failed = Some(format!("send: {e}"));
            break;
        }
        let deadline = Instant::now() + o.turn_timeout;
        loop {
            let idle = s.wait_idle(&mut ed, Duration::from_secs(1));
            for e in s.drain_events() {
                match e {
                    UiEvent::Text(x) => answer.push_str(&x),
                    UiEvent::Tool { ok: Some(true), .. } => m.steps += 1,
                    UiEvent::Tool { name, ok: Some(false), code, .. } => {
                        m.steps += 1;
                        // the tool name stays as the key older baselines have; typed refusals also
                        // count under `code:<code>`
                        if let Some(c) = code {
                            *m.refusals.entry(format!("code:{c}")).or_default() += 1;
                        }
                        *m.refusals.entry(name).or_default() += 1;
                    }
                    UiEvent::Usage(u) => {
                        m.tokens_in = u.session.input_tokens;
                        m.tokens_out = u.session.output_tokens;
                        m.usd = cost(o, m.tokens_in, m.tokens_out);
                        if !over_budget && cap_usd.is_some_and(|c| m.usd >= c) {
                            over_budget = true;
                            s.cancel();
                        }
                    }
                    UiEvent::Failed(e) => failed = Some(format!("{e}")),
                    _ => {}
                }
            }
            if idle {
                break;
            }
            if Instant::now() > deadline {
                failed = Some("the turn did not finish in time".into());
                s.cancel();
                break 'turns;
            }
        }
        if failed.is_some() || over_budget {
            break;
        }
    }
    let held = s.plan().and_then(|p| held_back(p.issues.iter().chain(&p.main_issues)));
    if let Some(h) = held {
        // the person would meet this block; "Allow large" must not hide a mass deletion
        failed = Some(h);
        if let Some(p) = s.plan() {
            receipts.push(receipt_of(p, 0, false, &case.id));
        }
    }
    if let Some(p) = s.plan() {
        if !p.items.is_empty() && failed.is_none() && !over_budget {
            if !p.unreviewed.is_empty() || s.redraft().is_some() {
                let rev = s.plan_rev();
                s.review(Some(rev));
                s.wait_idle(&mut ed, o.turn_timeout);
            }
            if !s.can_apply() {
                s.allow_large(true);
                s.wait_idle(&mut ed, o.turn_timeout);
            }
            let pv = s.plan().cloned();
            match s.apply(&mut ed) {
                Ok(op) => {
                    receipts.extend(pv.map(|p| receipt_of(&p, op.max(0) as u64, true, &case.id)));
                    s.wait_idle(&mut ed, o.turn_timeout);
                }
                Err(e) => {
                    receipts.extend(pv.map(|p| receipt_of(&p, 0, false, &case.id)));
                    failed = Some(format!("apply: {e}"));
                }
            }
        }
    }
    drop(s);
    m.undo_steps = op_rows(&ed) - ops0;

    let drift = made.replay.as_ref().and_then(|r| r.drift());
    let facts = match Facts::read(&ed.model) {
        Ok(f) => f,
        Err(e) => return fail(m, Status::Fail, format!("facts: {e}"), t0),
    };
    let ifc = if case.oracle.ifc.is_some() { ddc_io::ifc::export_ifc(&ed.model).ok() } else { None };
    let cx = EvalCx {
        ed: &ed,
        facts: &facts,
        before: before.as_ref(),
        answer: &answer,
        ifc: ifc.as_deref(),
        errors_before,
        receipts: &receipts,
    };
    m.oracles = evaluate(case, &cx);
    if let Some(dir) = &o.out {
        let _ = std::fs::create_dir_all(dir);
        let _ = ed.save_project_as(&dir.join(format!("{}.ddc", case.id)));
    }
    m.wall_s = t0.elapsed().as_secs_f64();
    m.status = if drift.is_some() {
        Status::Drift
    } else if failed.is_some() || over_budget || m.oracles.iter().any(|x| !x.pass) {
        Status::Fail
    } else {
        Status::Pass
    };
    m.note = drift
        .or(over_budget.then(|| "budget reached".to_string()))
        .or(failed)
        .unwrap_or_default();
    m
}

/// The order cases start in, as indices: non-smoke before smoke (a budget runs out on the simple
/// houses, not on the hard cases), and inside each, cases built on an earlier result (`@id`)
/// after the others. Empty phases are left out.
pub fn phases(cases: &[Case]) -> Vec<Vec<usize>> {
    let mut out = vec![];
    for smoke in [false, true] {
        for after in [false, true] {
            let p: Vec<usize> = (0..cases.len())
                .filter(|&i| (cases[i].tier == "smoke") == smoke && cases[i].base.starts_with('@') == after)
                .collect();
            if !p.is_empty() {
                out.push(p);
            }
        }
    }
    out
}

/// Runs the cases, `jobs` at a time, each in its own project, in the order of [`phases`]. After `budget_usd` is spent the remaining
/// cases are reported as not run. With `record_to`, each answered case keeps its recording.
pub fn run_all(
    cases: &[Case],
    factory: &dyn ProviderFactory,
    o: &Opts,
    jobs: usize,
    budget_usd: Option<f64>,
    record_to: Option<&Path>,
) -> Vec<CaseMetrics> {
    let left = Mutex::new(budget_usd);
    let out: Mutex<Vec<Option<CaseMetrics>>> = Mutex::new(vec![None; cases.len()]);
    for phase in &phases(cases) {
        let next = AtomicUsize::new(0);
        std::thread::scope(|sc| {
            for _ in 0..jobs.max(1).min(phase.len().max(1)) {
                let work = || loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&i) = phase.get(k) else { break };
                    let case = &cases[i];
                    let cap = *left.lock().unwrap();
                    let m = if cap.is_some_and(|c| c <= 0.0) {
                        CaseMetrics::not_run(&case.id, "budget spent")
                    } else {
                        match factory.make(case) {
                            Err(e) => CaseMetrics::not_run(&case.id, &e),
                            Ok(made) => {
                                let m = run_case(case, &made, o, cap);
                                if let (Some(dir), Some(rec)) = (record_to, &made.rec) {
                                    if let Err(e) = replay::save(dir, &case.id, &rec.recording()) {
                                        eprintln!("{}: recording not saved: {e}", case.id);
                                    }
                                }
                                if let Some(l) = left.lock().unwrap().as_mut() {
                                    *l -= m.usd;
                                }
                                m
                            }
                        }
                    };
                    out.lock().unwrap()[i] = Some(m);
                };
                crate::jobs::spawn_scoped_sized(sc, "ddc-eval", 64 << 20, work)
                    .expect("eval worker thread");
            }
        });
    }
    out.into_inner()
        .unwrap()
        .into_iter()
        .zip(cases)
        .map(|(m, c)| m.unwrap_or_else(|| CaseMetrics::not_run(&c.id, "not started")))
        .collect()
}

/// A block the runner never lifts: a mass deletion or a wipe by the agent. Plans that are only
/// large are still applied after "Allow large", as the person would.
pub fn held_back<'a>(issues: impl Iterator<Item = &'a PlanIssue>) -> Option<String> {
    issues
        .filter(|i| i.level == IssueLevel::Block)
        .find(|i| ["delete_large", "mass_delete", "wipe"].iter().any(|k| i.text_key.contains(k)))
        .map(|i| format!("blocked: {} ({})", i.text_key, i.args.join(" of ")))
}

/// The product's receipt of a plan (the one the agent's own commit answers with), token = the
/// undo step Apply returned. A plan that was not applied is blocked: when no rule names a mass
/// deletion, the count is what it would have deleted of what it touches.
fn receipt_of(p: &crate::ai::plan::PlanView, token: u64, applied: bool, label: &str) -> WriteReceipt {
    let issues: Vec<_> = p.issues.iter().chain(&p.main_issues).cloned().collect();
    let mut r = WriteReceipt::build(&p.diff, &p.qty, &issues, token, applied, label);
    if !applied && r.blocked.is_none() {
        r.blocked = Some((r.n_deleted, r.n_deleted + r.n_changed));
    }
    r
}
