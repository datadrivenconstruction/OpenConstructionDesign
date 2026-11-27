// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One request against a project without a window: the panel's own session (`UiSession`) driven
//! until it is idle, its plan applied only when asked. `ddc ai` and the eval runner use it.

use crate::ai::session::{UiEvent, CONFIRM_TTL};
use crate::ai::{AiError, SessionCfg, UiSession, ViewCtx};
use crate::editor::Editor;
use ddc_ai::gateway::{cost_of, Balance};
use ddc_ai::{Limits, Mode, Profile, Provider};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use web_time::{Duration, Instant};

/// Why a turn did not end on its own.
#[derive(Debug)]
pub enum DriveErr {
    Send(AiError),
    Timeout,
}

impl std::fmt::Display for DriveErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriveErr::Send(e) => write!(f, "send: {e}"),
            DriveErr::Timeout => f.write_str("the turn did not finish in time"),
        }
    }
}

/// Sends `say` and hands every event to `on` until the session is idle. `on` returning false
/// cancels the turn once (a budget reached); the events after it are still delivered.
pub fn drive_turn(
    s: &mut UiSession,
    ed: &mut Editor,
    say: String,
    view: ViewCtx,
    timeout: Duration,
    on: &mut dyn FnMut(UiEvent) -> bool,
) -> Result<(), DriveErr> {
    s.send_turn(ed, say, view, Mode::Propose)
        .map_err(DriveErr::Send)?;
    let deadline = Instant::now() + timeout;
    let mut cancelled = false;
    loop {
        let idle = s.wait_idle(ed, Duration::from_secs(1));
        for e in s.drain_events() {
            if !on(e) && !cancelled {
                cancelled = true;
                s.cancel();
            }
        }
        if idle {
            return Ok(());
        }
        if Instant::now() > deadline {
            s.cancel();
            return Err(DriveErr::Timeout);
        }
    }
}

/// Brings the card to where Apply is allowed: the review the person would confirm, and with
/// `allow_large` the large-change switch. Returns whether Apply may run now.
pub fn ready_to_apply(
    s: &mut UiSession,
    ed: &mut Editor,
    timeout: Duration,
    allow_large: bool,
) -> bool {
    let Some(p) = s.plan() else {
        return false;
    };
    if !p.unreviewed.is_empty() || s.redraft().is_some() {
        let rev = s.plan_rev();
        s.review(Some(rev));
        s.wait_idle(ed, timeout);
    }
    if !s.can_apply() && allow_large {
        s.allow_large(true);
        s.wait_idle(ed, timeout);
    }
    s.can_apply()
}

/// A plain view of the plan (the 2D plan, the current level, nothing picked).
pub fn plain_view(ed: &Editor) -> ViewCtx {
    ViewCtx {
        view: "plan".to_string(),
        level: ed.level,
        units: ed.tool.units,
        ..ViewCtx::default()
    }
}

pub struct Request {
    pub prompt: String,
    pub provider: Arc<dyn Provider>,
    pub profile: Profile,
    /// Stops the run when its cost at the profile prices reaches it.
    pub budget: Option<f64>,
    pub apply: bool,
    /// Lets a large change through, as the card's switch does.
    pub allow_large: bool,
    pub turn_timeout: Duration,
}

/// The money side before the first step: what one step may cost at most (a full context in, the
/// output limit out) and what the server says is left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Estimate {
    pub step_max: Option<f64>,
    pub balance: Option<Balance>,
}

pub fn estimate(profile: &Profile, provider: &dyn Provider) -> Estimate {
    let (pi, po) = (profile.price_in_per_mtok, profile.price_out_per_mtok);
    Estimate {
        step_max: cost_of(
            u64::from(profile.context_tokens),
            u64::from(profile.max_output_tokens),
            pi,
            po,
        ),
        balance: provider.balance().ok().flatten(),
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub estimate: Estimate,
    pub answer: String,
    /// Titles of the plan items, in card order.
    pub items: Vec<String>,
    /// Blocking issues of the card; `issue_lines` words them.
    pub blocked: Vec<crate::ai::validate::PlanIssue>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost: Option<f64>,
    pub over_budget: bool,
    /// The undo step of the applied plan.
    pub applied: Option<i64>,
    pub failed: Option<String>,
}

/// Opens a copy of `project` (never the file itself), runs `req` on it and, when the plan was
/// applied, saves the result to `out`. `None` project: an empty one.
pub fn run_project(project: Option<&Path>, out: Option<&Path>, req: &Request) -> Report {
    let mut r = Report::default();
    let mut ed = match open(project) {
        Ok(e) => e,
        Err(e) => {
            r.failed = Some(e);
            return r;
        }
    };
    let mut r = run(&mut ed, req);
    if let (Some(out), Some(_)) = (out, r.applied) {
        if let Err(e) = ed.save_project_as(out) {
            r.failed = Some(format!("{}: {e:?}", out.display()));
        }
    }
    r
}

fn open(project: Option<&Path>) -> Result<Editor, String> {
    let mut ed = Editor::new_in_memory().map_err(|e| format!("editor: {e:?}"))?;
    ed.reload().map_err(|e| format!("editor: {e:?}"))?;
    if let Some(src) = project {
        let copy = std::env::temp_dir().join(format!(
            "ddc_ai_{}_{}.ddc",
            std::process::id(),
            web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::copy(src, &copy).map_err(|e| format!("{}: {e}", src.display()))?;
        ed.open_project(&copy)
            .map_err(|e| format!("{}: {e:?}", src.display()))?;
    }
    Ok(ed)
}

/// The blocking issues as the card words them, in the language of a locale code (`en`, `de_DE`).
pub fn issue_lines(r: &Report, lang: &str) -> Vec<String> {
    let cat = ddc_ui::Catalog::new(ddc_ui::Lang::from_code(lang));
    r.blocked
        .iter()
        .map(|i| crate::ai::ui::logic::issue_text(&cat, i))
        .collect()
}

/// The request in a session on `ed`. A spent balance stops it before anything is sent.
pub fn run(ed: &mut Editor, req: &Request) -> Report {
    let mut r = Report::default();
    let local_only = crate::ai::ui::connect::policy_flag(&ed.model, "egress.local_only", false);
    if local_only && !ddc_ai::egress::is_local_url(&req.profile.base_url) {
        r.failed = Some("this project allows only a connection on this computer".into());
        return r;
    }
    r.estimate = estimate(&req.profile, req.provider.as_ref());
    if r.estimate
        .balance
        .and_then(|b| b.spendable())
        .is_some_and(|left| left <= 0.0)
    {
        r.failed = Some(ddc_ai::ProviderError::NoCredit("balance 0".into()).to_string());
        return r;
    }
    let cfg = SessionCfg {
        provider: req.provider.clone(),
        profile: req.profile.clone(),
        limits: Limits {
            max_cost: req.budget,
            ..Limits::default()
        },
        session: format!("cli-{}", std::process::id()),
        allow_snapshots: Arc::new(AtomicBool::new(false)),
        extra_generators: vec![],
        extra_rules: vec![],
        keep_text: false,
        confirm_ttl: CONFIRM_TTL,
        target: ddc_ai::catalog::Target::Main,
        auto_replan: true,
    };
    let mut s = match UiSession::start(ed, cfg, plain_view(ed)) {
        Ok(s) => s,
        Err(e) => {
            r.failed = Some(format!("session: {e}"));
            return r;
        }
    };
    if !s.wait_idle(ed, req.turn_timeout) {
        r.failed = Some("the session did not open".into());
        return r;
    }
    let budget = req.budget;
    let (mut failed, mut over) = (None::<String>, false);
    let mut on = |e: UiEvent| {
        match e {
            UiEvent::Text(x) => r.answer.push_str(&x),
            UiEvent::Usage(u) => {
                r.tokens_in = u.session.input_tokens;
                r.tokens_out = u.session.output_tokens;
                r.cost = u.cost;
                if budget.zip(u.cost).is_some_and(|(b, c)| c >= b) {
                    over = true;
                    return false;
                }
            }
            UiEvent::Failed(e) => failed = Some(e.to_string()),
            _ => {}
        }
        true
    };
    let view = plain_view(ed);
    if let Err(e) = drive_turn(
        &mut s,
        ed,
        req.prompt.clone(),
        view,
        req.turn_timeout,
        &mut on,
    ) {
        failed = Some(e.to_string());
    }
    r.over_budget = over;
    if let Some(p) = s.plan() {
        r.items = p.items.iter().map(|i| i.title.clone()).collect();
        r.blocked = p
            .issues
            .iter()
            .chain(&p.main_issues)
            .filter(|i| i.level == crate::ai::validate::IssueLevel::Block)
            .cloned()
            .collect();
    }
    if req.apply && failed.is_none() && !over && !r.items.is_empty() {
        if ready_to_apply(&mut s, ed, req.turn_timeout, req.allow_large) {
            match s.apply(ed) {
                Ok(op) => {
                    r.applied = Some(op);
                    s.wait_idle(ed, req.turn_timeout);
                }
                Err(e) => failed = Some(format!("apply: {e}")),
            }
        } else {
            failed = Some("the plan cannot be applied as it is".into());
        }
    }
    r.failed = failed;
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ai::fake::{Script, ScriptedProvider};
    use ddc_ai::{ChatRequest, Flow, ProviderError, StreamEvent, Usage};
    use serde_json::json;

    /// A model that draws one wall and then answers.
    fn wall() -> Vec<Vec<Script>> {
        vec![
            vec![Script::Tool {
                name: "create_elements".into(),
                args: json!({"title": "Wall", "cat": "wall", "items": [{"a": [0, 0], "b": [5, 0]}]}),
            }],
            vec![Script::Text("done".into())],
        ]
    }

    /// The scripted model behind a server that reports `left` as the balance.
    struct WithBalance(Arc<ScriptedProvider>, f64);

    impl Provider for WithBalance {
        fn stream(
            &self,
            req: &ChatRequest<'_>,
            sink: &mut dyn FnMut(StreamEvent) -> Flow,
        ) -> Result<(), ProviderError> {
            self.0.stream(req, sink)
        }
        fn list_models(&self) -> Result<Vec<String>, ProviderError> {
            self.0.list_models()
        }
        fn balance(&self) -> Result<Option<Balance>, ProviderError> {
            Ok(Some(Balance {
                account: Some(self.1),
                key: None,
            }))
        }
    }

    fn req(provider: Arc<dyn Provider>, apply: bool) -> Request {
        let mut profile = ddc_ai::fake::profile();
        profile.price_in_per_mtok = Some(1.0);
        profile.price_out_per_mtok = Some(2.0);
        Request {
            prompt: "draw a wall".into(),
            provider,
            profile,
            budget: None,
            apply,
            allow_large: false,
            turn_timeout: Duration::from_secs(120),
        }
    }

    fn ops(ed: &Editor) -> i64 {
        ed.model
            .sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
            .unwrap_or(0)
    }

    fn editor() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.reload().unwrap();
        ed
    }

    #[test]
    fn a_request_shows_its_plan_and_applies_only_when_asked() {
        let mut ed = editor();
        let before = ops(&ed);
        let r = run(&mut ed, &req(ScriptedProvider::new(wall()), false));
        assert_eq!(r.failed, None);
        assert_eq!(r.items.len(), 1, "{r:?}");
        assert_eq!(
            (r.applied, ops(&ed)),
            (None, before),
            "nothing is written without apply"
        );
        assert!(r.tokens_in > 0 && r.cost.is_some(), "{r:?}");
        assert!(
            r.estimate.step_max.is_some(),
            "the profile prices give a step estimate"
        );

        let r = run(&mut ed, &req(ScriptedProvider::new(wall()), true));
        assert_eq!(r.failed, None, "{r:?}");
        assert!(r.applied.is_some());
        assert_eq!(ops(&ed), before + 1, "one undo step");
    }

    #[test]
    fn the_project_file_is_copied_and_the_result_goes_to_out() {
        let dir = std::env::temp_dir().join(format!("ddc_headless_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (src, out) = (dir.join("in.ddc"), dir.join("out.ddc"));
        editor().save_project_as(&src).unwrap();
        let size = std::fs::metadata(&src).unwrap().len();
        let r = run_project(
            Some(&src),
            Some(&out),
            &req(ScriptedProvider::new(wall()), true),
        );
        assert_eq!(r.failed, None, "{r:?}");
        assert!(out.is_file());
        assert_eq!(
            std::fs::metadata(&src).unwrap().len(),
            size,
            "the input is never written"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_balance_stops_before_anything_is_sent_and_a_budget_stops_the_run() {
        let mut ed = editor();
        let scripted = ScriptedProvider::new(wall());
        let r = run(
            &mut ed,
            &req(Arc::new(WithBalance(scripted.clone(), 0.0)), true),
        );
        assert!(
            r.failed
                .as_deref()
                .is_some_and(|f| f.starts_with("account balance used up")),
            "{r:?}"
        );
        assert!(scripted.requests().is_empty(), "no request left");

        let big = Usage {
            input_tokens: 2_000_000,
            output_tokens: 0,
            ..Usage::default()
        };
        let mut q = req(
            Arc::new(WithBalance(ScriptedProvider::with_usage(wall(), big), 5.0)),
            true,
        );
        q.budget = Some(1.0);
        let before = ops(&ed);
        let r = run(&mut ed, &q);
        assert_eq!(r.estimate.balance.and_then(|b| b.spendable()), Some(5.0));
        assert!(r.over_budget || r.failed.is_some(), "{r:?}");
        assert_eq!(ops(&ed), before, "a run over its budget applies nothing");
    }
}
