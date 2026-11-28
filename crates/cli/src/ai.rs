// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc ai "<request>" --project in.ddc --out out.ddc --profile p.json [--budget-usd N] [--apply]`:
//! the editor's assistant on a project without the window. It runs the panel's own session
//! (`ddc_app::ai::headless`); the plan is printed, and written to `--out` only with `--apply`.
//! The key comes from DDC_AI_KEY, else from the system key store under the profile id.

use std::ffi::OsString;
use std::path::PathBuf;

use ddc_ai::{build_provider, consent_key, AuthKind, EgressGuard, Profile, Secret};
use ddc_app::ai::headless::{self, Report, Request};
use serde_json::json;
use web_time::Duration;

use crate::ai_profile::{number, value, KEY_ENV};
use crate::{progress, EXIT_FAILED, EXIT_OK, EXIT_PARTIAL, LOG_SCHEMA};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AiArgs {
    pub prompt: String,
    pub project: Option<PathBuf>,
    pub out: Option<PathBuf>,
    /// A profile file (the JSON of one connection), or the id or name of a stored one.
    pub profile: String,
    pub budget_usd: Option<f64>,
    /// Per-turn token and time limits; unset keeps the stock ones.
    pub max_tokens: Option<u64>,
    pub max_time_s: Option<u32>,
    /// Lifts steps, calls, tokens, time and plan size for the run (also `DDC_AI_UNLIMITED=1`).
    pub unlimited: bool,
    /// Turns that go on by themselves after a stop at steps, calls, tokens or time.
    pub auto_continue: u32,
    /// Applies a plan that a limit cut short; otherwise it is printed and kept.
    pub apply_partial: bool,
    pub apply: bool,
    pub allow_large: bool,
    pub timeout_s: u64,
}

pub const USAGE: &str = "\
  ddc ai \"<request>\" --profile <file|id|name> [--project <in.ddc>] [--out <out.ddc>]
      [--budget-usd <N>] [--max-tokens <N>] [--max-time-s <N>] [--ai-unlimited]
      [--auto-continue <N>] [--apply] [--apply-partial] [--allow-large] [--timeout-s <N>]
      The editor's assistant without the window. The plan is printed; with --apply it is applied
      as one undo step and the project saved to --out (the input is never overwritten).
      --profile: a connection file (as in ai_profiles.json) or a stored connection
      (ddc ai-profile list). The key: DDC_AI_KEY, else the system key store.
      --budget-usd stops the run when its cost at the connection prices reaches N.
      --max-tokens and --max-time-s bound one turn; a turn that hits
      a limit says which and how to raise it.
      --ai-unlimited (or DDC_AI_UNLIMITED=1) lifts steps, calls, tokens, time and plan size;
      the --budget-usd cap and the confirmation of mass deletes stay.
      --auto-continue N goes on up to N times after a stop at a limit. A plan that a limit cut
      short is not applied unless --apply-partial is given.
      Before the run the cost of one step at most and the balance (when the server reports it)
      are printed.
";

/// `rest` is what follows `ai`; gives the command, `--json` and `--quiet`.
pub fn parse(rest: &[OsString]) -> Result<(AiArgs, bool, bool), String> {
    let (mut json, mut quiet) = (false, false);
    let mut a = AiArgs {
        timeout_s: 1800,
        ..AiArgs::default()
    };
    let mut words: Vec<String> = vec![];
    let mut it = rest.iter().map(|s| s.to_string_lossy().into_owned());
    let it: &mut dyn Iterator<Item = String> = &mut it;
    while let Some(w) = it.next() {
        match w.as_str() {
            "--json" => json = true,
            "--quiet" | "-q" => quiet = true,
            "--project" => a.project = Some(value(it, "--project")?.into()),
            "--out" | "-o" => a.out = Some(value(it, "--out")?.into()),
            "--profile" => a.profile = value(it, "--profile")?,
            "--budget-usd" => a.budget_usd = Some(number(it, "--budget-usd")?),
            "--max-tokens" => a.max_tokens = Some(number(it, "--max-tokens")? as u64),
            "--max-time-s" => a.max_time_s = Some(number(it, "--max-time-s")? as u32),
            "--ai-unlimited" => a.unlimited = true,
            "--auto-continue" => a.auto_continue = number(it, "--auto-continue")? as u32,
            "--apply-partial" => a.apply_partial = true,
            "--apply" => a.apply = true,
            "--allow-large" => a.allow_large = true,
            "--timeout-s" => a.timeout_s = number(it, "--timeout-s")? as u64,
            f if f.starts_with("--") => return Err(format!("unknown option {f} for ai")),
            _ => words.push(w),
        }
    }
    a.prompt = words.join(" ").trim().to_string();
    if a.prompt.is_empty() {
        return Err(format!("ai needs a request\n{USAGE}"));
    }
    if a.profile.is_empty() {
        return Err("ai needs --profile".into());
    }
    if a.apply && a.out.is_none() {
        return Err("--apply needs --out: the input project is never overwritten".into());
    }
    if a.budget_usd.is_some_and(|b| b.is_nan() || b <= 0.0) {
        return Err("--budget-usd needs a positive number".into());
    }
    if a.max_tokens == Some(0) || a.max_time_s == Some(0) {
        return Err("--max-tokens and --max-time-s need a positive number".into());
    }
    if a.project.is_some() && a.project == a.out {
        return Err("--out must differ from --project".into());
    }
    Ok((a, json, quiet))
}

/// The connection: a profile file, else a stored one by id or name.
pub fn load_profile(which: &str) -> Result<Profile, String> {
    let path = std::path::Path::new(which);
    if path.is_file() {
        let t = std::fs::read_to_string(path).map_err(|e| format!("{which}: {e}"))?;
        return serde_json::from_str(&t).map_err(|e| format!("{which}: not a connection: {e}"));
    }
    let dir = crate::ai_profile::data_dir()?;
    let list =
        ddc_ai::profile::load_profiles(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    list.into_iter()
        .find(|p| p.id == which || p.name == which)
        .ok_or(format!("no connection file or stored connection {which}"))
}

/// The key from `env`, else from the store; none for a connection without a key.
pub fn key_for(
    p: &Profile,
    env: Option<String>,
    store: &dyn ddc_ai::SecretStore,
) -> Result<Option<Secret>, String> {
    if p.auth == AuthKind::None {
        return Ok(None);
    }
    if let Some(k) = env.filter(|k| !k.trim().is_empty()) {
        return Ok(Some(Secret::new(k.trim())));
    }
    match store.get(&p.id) {
        Ok(Some(k)) => Ok(Some(k)),
        _ => Err(format!("set {KEY_ENV} to the key of this connection")),
    }
}

/// The request as the command line gives it, with the provider already built.
pub fn request(
    a: &AiArgs,
    profile: Profile,
    provider: std::sync::Arc<dyn ddc_ai::Provider>,
) -> Request {
    Request {
        prompt: a.prompt.clone(),
        provider,
        profile,
        budget: a.budget_usd,
        apply: a.apply,
        allow_large: a.allow_large,
        turn_timeout: Duration::from_secs(a.timeout_s.max(10)),
        max_tokens: a.max_tokens,
        max_time_s: a.max_time_s,
        unlimited: a.unlimited,
        auto_continue: a.auto_continue,
        apply_partial: a.apply_partial,
    }
}

fn money(v: f64) -> String {
    format!("{v:.2}")
}

/// What is printed before the run: the step estimate and the balance.
pub fn estimate_lines(r: &Report, budget: Option<f64>) -> Vec<String> {
    let mut out = vec![];
    if let Some(s) = r.estimate.step_max {
        out.push(format!(
            "one step costs at most {} at the connection prices",
            money(s)
        ));
    }
    if let Some(b) = r.estimate.balance {
        if let Some(a) = b.account {
            out.push(format!("balance on the account: {}", money(a)));
        }
        if let Some(k) = b.key {
            out.push(format!("left under the key's limit: {}", money(k)));
        }
        if let (Some(left), Some(cap)) = (b.spendable(), budget) {
            if left < cap {
                out.push(format!(
                    "note: the budget {} is more than what is left",
                    money(cap)
                ));
            }
        }
    }
    out
}

/// The exit code of a report: done, a plan shown but not applied, or nothing done.
pub fn exit_of(r: &Report, apply: bool) -> i32 {
    if r.failed.is_some() {
        EXIT_FAILED
    } else if (apply && r.applied.is_none()) || r.over_budget || r.limit.is_some() || r.held_partial {
        EXIT_PARTIAL
    } else {
        EXIT_OK
    }
}

/// Which limit ended the turn, its value, and the flag that raises it.
pub fn limit_line(a: &AiArgs, what: &str) -> String {
    let d = ddc_ai::Limits::default();
    match what {
        "tokens" => format!(
            "stopped: the token limit per turn ({}) was reached; the work so far is in the plan above; raise it with --max-tokens N, or use --ai-unlimited",
            a.max_tokens.unwrap_or(d.max_turn_tokens)
        ),
        "wall" => format!(
            "stopped: the time limit per turn ({} s) was reached; the work so far is in the plan above; raise it with --max-time-s N, or use --ai-unlimited",
            a.max_time_s.unwrap_or(d.max_wall_s)
        ),
        "cost" => "stopped: the cost limit was reached; raise it with --budget-usd N".to_string(),
        "output" => "stopped: the model's answer kept hitting its output limit; raise it with --max-output N (ddc ai-profile)".to_string(),
        "steps" | "tool_calls" => format!(
            "stopped: the {} limit per turn was reached; the work so far is in the plan above; go on with --auto-continue N, or lift it with --ai-unlimited",
            if what == "steps" { "step" } else { "tool call" }
        ),
        other => format!("stopped: limit '{other}' was reached; narrow the request or ask again to continue"),
    }
}

pub fn render(a: &AiArgs, r: &Report, lang: &str) -> String {
    let mut t = String::new();
    for l in estimate_lines(r, a.budget_usd) {
        t.push_str(&l);
        t.push('\n');
    }
    if !r.answer.trim().is_empty() {
        t.push_str(r.answer.trim());
        t.push('\n');
    }
    if !r.items.is_empty() {
        t.push_str(&format!("plan: {} item(s)\n", r.items.len()));
        for i in &r.items {
            t.push_str(&format!("  - {i}\n"));
        }
    }
    for b in headless::issue_lines(r, lang) {
        t.push_str(&format!("blocked: {b}\n"));
    }
    let cost = r
        .cost
        .map(|c| format!(", cost {}", money(c)))
        .unwrap_or_default();
    t.push_str(&format!(
        "tokens: {} in, {} out{cost}\n",
        r.tokens_in, r.tokens_out
    ));
    if r.over_budget {
        t.push_str("stopped: the budget was reached\n");
    }
    if let Some(l) = r.limit {
        t.push_str(&limit_line(a, l));
        t.push('\n');
    }
    match (&r.failed, r.applied, &a.out) {
        (Some(e), _, _) => t.push_str(&format!("error: {}\n", ddc_ai::secret::scrub_known(e))),
        (None, Some(_), Some(out)) => {
            t.push_str(&format!("ok: applied, saved {}\n", out.display()))
        }
        (None, None, _) if r.held_partial => t.push_str(
            "not applied: the plan is incomplete, a limit stopped the run; add --auto-continue N, raise the limit or use --ai-unlimited, or --apply-partial to apply it as it is
",
        ),
        (None, None, _) if !r.items.is_empty() => {
            t.push_str("not applied: add --apply --out <file> to apply\n")
        }
        _ => {}
    }
    t
}

pub fn report_json(a: &AiArgs, r: &Report, code: i32) -> serde_json::Value {
    json!({
        "schema": LOG_SCHEMA, "command": "ai", "exit": code,
        "estimate": {"step_max": r.estimate.step_max,
                     "balance": r.estimate.balance.map(|b| json!({"account": b.account, "key": b.key}))},
        "answer": r.answer, "items": r.items,
        "blocked": headless::issue_lines(r, "en"),
        "tokens_in": r.tokens_in, "tokens_out": r.tokens_out, "cost": r.cost,
        "over_budget": r.over_budget, "limit": r.limit, "partial_held": r.held_partial, "applied": r.applied.is_some(),
        "out": a.out.as_ref().filter(|_| r.applied.is_some()).map(|p| p.to_string_lossy().into_owned()),
        "error": r.failed.as_deref().map(ddc_ai::secret::scrub_known),
    })
}

pub fn run(a: &AiArgs, json_out: bool, quiet: bool) -> (String, i32) {
    let fail = |e: String| -> (String, i32) {
        let e = ddc_ai::secret::scrub_known(&e);
        if json_out {
            (
                json!({"schema": LOG_SCHEMA, "command": "ai", "exit": EXIT_FAILED, "error": e})
                    .to_string(),
                EXIT_FAILED,
            )
        } else {
            (format!("error: {e}\n"), EXIT_FAILED)
        }
    };
    let profile = match load_profile(&a.profile) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let store = ddc_ai::secret::default_store();
    let key = match key_for(&profile, std::env::var(KEY_ENV).ok(), &*store) {
        Ok(k) => k,
        Err(e) => return fail(e),
    };
    // naming the connection on the command line is the consent to send there
    let host = match consent_key(&profile.base_url) {
        Ok(h) => h,
        Err(e) => return fail(e.to_string()),
    };
    let guard = EgressGuard {
        local_only: false,
        consented_host: Some(host),
    };
    let provider = match build_provider(&profile, key, guard) {
        Ok(p) => p,
        Err(e) => return fail(e.to_string()),
    };
    progress(
        quiet,
        &format!("asking {} ({})", profile.name, profile.model),
    );
    let req = request(a, profile, provider);
    let r = headless::run_project(
        a.project.as_deref(),
        a.out.as_deref().filter(|_| a.apply),
        &req,
    );
    let code = exit_of(&r, a.apply);
    if json_out {
        return (report_json(a, &r, code).to_string(), code);
    }
    let lang = std::env::var("LANG").unwrap_or_else(|_| "en".into());
    (render(a, &r, &lang), code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ai::gateway::Balance;
    use ddc_ai::MemoryStore;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_unlimited_flags_reach_the_request_and_a_cut_plan_is_held() {
        let (a, _, _) = parse(&os(&[
            "draw", "--profile", "p", "--ai-unlimited", "--auto-continue", "3", "--apply", "--out", "o.ddc",
        ]))
        .unwrap();
        assert!(a.unlimited && a.auto_continue == 3 && !a.apply_partial);
        let (b, _, _) = parse(&os(&["draw", "--profile", "p", "--apply-partial"])).unwrap();
        assert!(b.apply_partial && !b.unlimited);
        let held = Report { held_partial: true, items: vec!["wall".into()], limit: Some("tool_calls"), ..Report::default() };
        assert_eq!(exit_of(&held, true), EXIT_PARTIAL);
        let text = render(&a, &held, "en");
        assert!(text.contains("not applied: the plan is incomplete"), "{text}");
        assert!(text.contains("--auto-continue"), "{text}");
        assert!(report_json(&a, &held, EXIT_PARTIAL)["partial_held"].as_bool().unwrap());
    }

    #[test]
    fn parse_takes_the_request_and_guards_the_input() {
        let (a, json, _) = parse(&os(&[
            "draw",
            "a wall",
            "--profile",
            "p.json",
            "--project",
            "in.ddc",
            "--out",
            "o.ddc",
            "--apply",
            "--budget-usd",
            "0.5",
            "--json",
        ]))
        .unwrap();
        assert_eq!(a.prompt, "draw a wall");
        assert_eq!((a.apply, a.budget_usd, json), (true, Some(0.5), true));
        assert!(
            parse(&os(&["x", "--profile", "p", "--apply"])).is_err(),
            "apply needs out"
        );
        assert!(parse(&os(&[
            "x",
            "--profile",
            "p",
            "--project",
            "a.ddc",
            "--out",
            "a.ddc"
        ]))
        .is_err());
        assert!(
            parse(&os(&["--profile", "p"])).is_err(),
            "a request is needed"
        );
        assert!(parse(&os(&["x"])).is_err(), "a profile is needed");
        assert!(parse(&os(&["x", "--profile", "p", "--budget-usd", "-1"])).is_err());
    }

    #[test]
    fn the_key_comes_from_the_environment_then_the_store() {
        let mut p = ddc_ai::fake::profile();
        let store = MemoryStore::new();
        assert_eq!(
            key_for(&p, None, &store).unwrap(),
            None,
            "no key for a keyless connection"
        );
        p.auth = AuthKind::Bearer;
        assert!(key_for(&p, None, &store).unwrap_err().contains(KEY_ENV));
        ddc_ai::SecretStore::set(&store, &p.id, Secret::new("stored")).unwrap();
        assert_eq!(
            key_for(&p, None, &store).unwrap(),
            Some(Secret::new("stored"))
        );
        assert_eq!(
            key_for(&p, Some("env".into()), &store).unwrap(),
            Some(Secret::new("env"))
        );
    }

    #[test]
    fn turn_limits_parse_and_a_stop_names_the_limit() {
        let os = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        let (a, _, _) = parse(&os(&["x", "--profile", "p", "--max-tokens", "900000", "--max-time-s", "1800"])).unwrap();
        assert_eq!((a.max_tokens, a.max_time_s), (Some(900_000), Some(1800)));
        assert!(parse(&os(&["x", "--profile", "p", "--max-tokens", "0"])).is_err());
        let mut r = Report::default();
        r.limit = Some("tokens");
        let t = render(&a, &r, "en");
        assert!(t.contains("900000") && t.contains("--max-tokens"), "{t}");
        assert_eq!(exit_of(&r, false), EXIT_PARTIAL);
        r.limit = Some("wall");
        assert!(render(&AiArgs::default(), &r, "en").contains("600 s"));
    }

    #[test]
    fn estimate_and_balance_are_said_before_the_plan() {
        let mut r = Report::default();
        r.estimate.step_max = Some(0.25);
        r.estimate.balance = Some(Balance {
            account: Some(3.0),
            key: Some(0.5),
        });
        let lines = estimate_lines(&r, Some(1.0));
        assert!(lines[0].contains("0.25"));
        assert!(
            lines.iter().any(|l| l.contains("3.00")) && lines.iter().any(|l| l.contains("0.50"))
        );
        assert!(
            lines.iter().any(|l| l.starts_with("note:")),
            "a budget over the balance is said"
        );
        r.items = vec!["Wall".into()];
        let a = AiArgs {
            prompt: "x".into(),
            ..AiArgs::default()
        };
        let t = render(&a, &r, "en");
        assert!(t.find("0.25").unwrap() < t.find("plan:").unwrap(), "{t}");
        assert!(t.contains("not applied"));
        assert_eq!(exit_of(&r, false), EXIT_OK);
        assert_eq!(exit_of(&r, true), EXIT_PARTIAL);
        r.failed = Some("account balance used up: x".into());
        assert_eq!(exit_of(&r, false), EXIT_FAILED);
    }
}
