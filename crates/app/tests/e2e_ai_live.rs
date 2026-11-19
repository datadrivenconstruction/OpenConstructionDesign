// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Live measurement of the assistant on hard briefs: the real provider, every tool, many steps,
//! the plan applied and the project saved, scored objectively against each task's `expect`.
//!
//! DDC_LIVE_PROFILE=profile.json DDC_LIVE_OUT=dir DDC_LIVE_BUDGET=usd DDC_AI_KEY=...
//! [DDC_LIVE_TASKS=tasks.json] [DDC_LIVE_BASE=base.ddc] [DDC_LIVE_ONLY=id,id] [DDC_LIVE_MODEL=id]
//! [DDC_LIVE_TASK_USD=cap] [DDC_LIVE_KEY_ENV=fallback variable]
//! cargo test -p ddc-app --test e2e_ai_live -- --ignored
//! Without DDC_LIVE_BASE the HVAC and question tasks run on a 12-storey office drafted by the
//! generator itself (scripted, no API), saved once as `live_base.ddc`.
//! DDC_LIVE_TASKS=builtin runs the built-in footprint briefs (triangle plot, round tower,
//! crescent, 30 degree L, trapezoid plot); DDC_LIVE_FOOTPRINTS=1 adds them to a task file.
mod e2e_ai_common;
mod e2e_common;
#[path = "../../ai/examples/common/live_score.rs"]
mod live_score;

use ddc_ai::{build_provider, AuthKind, EgressGuard, Limits, Mode, Profile, Protocol, Secret, ToolCalling};
use ddc_app::ai::session::UiEvent;
use ddc_app::ai::UiSession;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::{json, Value as J};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const LABELS: &str = include_str!("../../ai/examples/common/report_labels.json");

/// Passes requests on and keeps every tool call of the run with its arguments and the result
/// the model read (refusal detail included), so a ledger line can say why a call failed.
struct Trace {
    inner: std::sync::Arc<dyn ddc_ai::provider::Provider>,
    calls: std::sync::Mutex<Vec<(String, String, J)>>,
    results: std::sync::Mutex<std::collections::HashMap<String, (String, bool)>>,
}

impl Trace {
    fn new(inner: std::sync::Arc<dyn ddc_ai::provider::Provider>) -> std::sync::Arc<Trace> {
        std::sync::Arc::new(Trace { inner, calls: Default::default(), results: Default::default() })
    }

    fn rows(&self) -> Vec<J> {
        let cut = |s: &str, n: usize| s.chars().take(n).collect::<String>();
        let results = self.results.lock().unwrap();
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(id, name, args)| {
                let (out, err) = results.get(id).cloned().unwrap_or_default();
                json!({"name": name, "args": cut(&args.to_string(), 600), "error": err, "result": cut(&out, 900)})
            })
            .collect()
    }
}

impl ddc_ai::provider::Provider for Trace {
    fn stream(
        &self,
        req: &ddc_ai::provider::ChatRequest<'_>,
        sink: &mut dyn FnMut(ddc_ai::provider::StreamEvent) -> ddc_ai::provider::Flow,
    ) -> Result<(), ddc_ai::provider::ProviderError> {
        use ddc_ai::provider::Part;
        let (mut calls, mut results) = (self.calls.lock().unwrap(), self.results.lock().unwrap());
        for p in req.messages.iter().flat_map(|m| &m.parts) {
            match p {
                Part::ToolCall { id, name, args } if !calls.iter().any(|c| &c.0 == id) => {
                    calls.push((id.clone(), name.clone(), args.clone()));
                }
                // the first time a result is sent it is whole; later requests may compact it
                Part::ToolResult { id, content, is_error, .. } if !results.contains_key(id) => {
                    results.insert(id.clone(), (content.clone(), *is_error));
                }
                _ => {}
            }
        }
        drop((calls, results));
        self.inner.stream(req, sink)
    }

    fn list_models(&self) -> Result<Vec<String>, ddc_ai::provider::ProviderError> {
        self.inner.list_models()
    }
}

/// Rows per table, and the most frequent values of naming columns, so a checklist can be
/// judged from the model itself.
fn summary(ed: &Editor) -> J {
    let db = ed.model.sql();
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|t| t.unwrap())
        .filter(|t| !NOT_MODEL.contains(&t.as_str()) && !t.starts_with("sqlite_"))
        .collect();
    let mut out = serde_json::Map::new();
    for t in tables {
        let n: i64 = db.query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0)).unwrap_or(0);
        if n == 0 {
            continue;
        }
        let cols: Vec<String> = db
            .prepare(&format!("SELECT name FROM pragma_table_info('{t}')"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|c| c.unwrap())
            .collect();
        let mut tops = serde_json::Map::new();
        for c in cols.iter().filter(|c| {
            ["name", "kind", "family", "category", "type_name", "system", "shape", "profile", "role"]
                .contains(&c.as_str())
        }) {
            let q = format!(
                "SELECT CAST(\"{c}\" AS TEXT), count(*) FROM \"{t}\" GROUP BY 1 ORDER BY 2 DESC LIMIT 12"
            );
            let vals: Vec<J> = db
                .prepare(&q)
                .unwrap()
                .query_map([], |r| Ok(json!([r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?])))
                .unwrap()
                .filter_map(Result::ok)
                .collect();
            tops.insert(c.clone(), J::Array(vals));
        }
        out.insert(t, json!({"rows": n, "top": tops}));
    }
    J::Object(out)
}

fn wait(s: &mut UiSession, ed: &mut Editor, limit: Duration) -> bool {
    s.wait_idle(ed, limit)
}

/// Every project stored in the model, rebuilt from its spec: the rules in force, the ten most
/// telling decisions with their rule, gates, layout errors and the brief check.
fn stored_projects(ed: &Editor) -> Vec<J> {
    use ddc_gen::project;
    let list = ddc_app::ai::project_plan::stored(&ed.model).unwrap_or_default();
    list.into_iter()
        .map(|(pid, rec)| {
            let input = rec["input"].clone();
            // a spec on the model's terrain reads it, as the build did
            let read = ddc_app::ai::project_plan::with_model_terrain(&ed.model, &input);
            let spec = match project::parse_spec(&read) {
                Ok(s) => s,
                Err(e) => return json!({"pid": pid, "spec": input, "error": e.to_string()}),
            };
            let p = match project::build_project(&spec) {
                Ok(p) => p,
                Err(e) => return json!({"pid": pid, "spec": input, "error": e.to_string()}),
            };
            let g = project::check_project(&spec, &p);
            let layout: Vec<String> = p
                .storeys
                .iter()
                .enumerate()
                .filter_map(|(k, st)| ddc_gen::validate_layout(&st.layout).err().map(|e| format!("floor {}: {e}", k + 1)))
                .collect();
            let metrics: serde_json::Map<String, J> = p.metrics.iter().map(|m| (m.0.to_string(), json!(m.1))).collect();
            // built rooms as [kind, m2]: the checks read kinds, never names in some language
            let area = |poly: &[(f64, f64)]| {
                let n = poly.len();
                (0..n).map(|i| poly[i].0 * poly[(i + 1) % n].1 - poly[(i + 1) % n].0 * poly[i].1).sum::<f64>().abs() / 2.0
            };
            let rooms: Vec<J> = p
                .storeys
                .iter()
                .chain(&p.basements)
                .flat_map(|st| st.layout.rooms.iter().zip(&st.rooms))
                .map(|(g, r)| json!([r.kind.key(), (area(&g.polygon) * 100.0).round() / 100.0]))
                .collect();
            json!({"pid": pid, "spec": input, "rules": spec.rules.to_json(),
                   "why": project::why::top(&spec, &p, 10), "gates": g.failures, "warnings": g.warnings,
                   "layout_errors": layout, "brief_check": project::brief::check(&spec, &p).report,
                   "storeys": p.storeys.len(), "footprint": [p.footprint.0, p.footprint.1],
                   "apartments": p.apartments, "metrics": metrics, "rooms": rooms})
        })
        .collect()
}

/// Hard briefs on footprints that are not bars; `expect` is what a reviewer checks in the
/// saved project.
fn footprint_tasks() -> Vec<J> {
    vec![
        json!({"id": "fp_triangle_plot", "brief": "Office building, 4 floors, on a triangular plot with corners (0,0), (60,0) and (25,48) metres; keep 3 m from the plot boundary. Flat roof.",
               "expect": "footprint.plot with the three corners and plot_setback 3; triangular slabs; rooms along all three facades"}),
        json!({"id": "fp_round_tower", "brief": "Round residential tower, 36 m diameter, 12 floors, apartments of 2 and 3 rooms, lift, hip roof.",
               "expect": "footprint.shape circle width 36 (or a polygon with bulges); curved facade as chords; central core; roof kind hip"}),
        json!({"id": "fp_crescent", "brief": "School shaped as a crescent, 80 m across and 34 m deep, wings 16 m thick, 3 floors, classrooms on the outer curve.",
               "expect": "footprint.shape crescent with thickness 16; classrooms windowed; no straight-bar massing"}),
        json!({"id": "fp_l_30deg", "brief": "Apartment block with two 15 m deep wings meeting at 30 degrees instead of a right angle, wings about 50 m long, 6 floors, gable roof.",
               "expect": "footprint.shape l with angle 30 and arm 15 (or the polygon); roof kind gable; core in a band where the plate is thin"}),
        json!({"id": "fp_trapezoid_plot", "brief": "Fit an office on this map plot, 5 m from its edges: [[0,0],[70,0],[58,40],[6,36]]. 5 floors, each floor above the third set back 2 m, a slight twist of 2 degrees per floor.",
               "expect": "footprint.plot + plot_setback 5; design.levels from 3 setback 2; design.twist 2; the stair at the same place on every floor"}),
    ]
}

/// The office the HVAC and question tasks work on: drafted by the generator through a scripted
/// call and applied, so every run starts from the same model.
fn draft_base_office(ed: &mut Editor) {
    let spec = json!({"typology": "office", "floors": 12, "lift": true, "lang": "en", "name": "Live base office"});
    let (mut s, provider) = run(ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "base office: {results:?}");
    apply_and_check(&mut s, ed);
}

fn fresh_editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

#[test]
fn the_live_base_office_drafts_and_applies() {
    let mut ed = fresh_editor();
    draft_base_office(&mut ed);
    assert!(e2e_common::count(&ed.model, "level") >= 12);
    assert!(e2e_common::count(&ed.model, "room") > 0);
}

#[test]
fn every_live_task_has_an_expectation() {
    let list: Vec<J> = serde_json::from_str(include_str!("../../ai/examples/live_tasks.json")).unwrap();
    assert_eq!(list.len(), 16);
    for t in &list {
        assert!(t["expect"].is_object(), "{}", t["id"]);
    }
}

#[test]
#[ignore = "spends API credit; run with DDC_LIVE_PROFILE, DDC_LIVE_OUT, DDC_LIVE_BUDGET and DDC_AI_KEY"]
fn live_hard_tasks() {
    let (Ok(pf), Ok(out), Some(budget)) = (
        std::env::var("DDC_LIVE_PROFILE"),
        std::env::var("DDC_LIVE_OUT"),
        std::env::var("DDC_LIVE_BUDGET").ok().and_then(|b| b.parse::<f64>().ok()),
    ) else {
        eprintln!("set DDC_LIVE_PROFILE, DDC_LIVE_OUT and DDC_LIVE_BUDGET");
        return;
    };
    let pf = live_score::load_profile(&pf).unwrap();
    let fallback = std::env::var("DDC_LIVE_KEY_ENV").ok();
    let mut names = vec!["DDC_AI_KEY"];
    if let Some(f) = fallback.as_deref() {
        names.push(f);
    }
    let Some(key) = live_score::read_key(&names) else {
        eprintln!("no key: set DDC_AI_KEY (or name another variable with DDC_LIVE_KEY_ENV)");
        return;
    };
    let prices = live_score::Prices::of(&pf["prices"]);
    let tasks = std::env::var("DDC_LIVE_TASKS")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../ai/examples/live_tasks.json").to_string());
    let base = std::env::var("DDC_LIVE_BASE").ok();
    let only: Vec<String> = std::env::var("DDC_LIVE_ONLY")
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let per_task: f64 = std::env::var("DDC_LIVE_TASK_USD").ok().and_then(|b| b.parse().ok()).unwrap_or(1.2);
    let model = std::env::var("DDC_LIVE_MODEL").unwrap_or_else(|_| pf["model"].as_str().unwrap().to_string());
    // DDC_LIVE_TASKS=builtin runs the footprint briefs alone; DDC_LIVE_FOOTPRINTS=1 adds them
    let mut list: Vec<J> = if tasks == "builtin" {
        vec![]
    } else {
        serde_json::from_str(&std::fs::read_to_string(&tasks).unwrap()).unwrap()
    };
    if tasks == "builtin" || std::env::var("DDC_LIVE_FOOTPRINTS").is_ok() {
        list.extend(footprint_tasks());
    }
    std::fs::create_dir_all(&out).unwrap();
    let base = base.unwrap_or_else(|| {
        let file = Path::new(&out).join("live_base.ddc");
        if !file.exists() {
            let mut ed = fresh_editor();
            draft_base_office(&mut ed);
            ed.save_project_as(&file).unwrap();
        }
        file.to_string_lossy().into_owned()
    });
    let base_url = pf["base_url"].as_str().unwrap().to_string();
    let host = base_url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("").to_string();
    let profile = Profile {
        id: "live".into(),
        name: "live".into(),
        protocol: Protocol::ContentBlocks,
        base_url,
        model: model.clone(),
        auth: AuthKind::Header { name: pf["auth_header"].as_str().unwrap().to_string() },
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
        price_in_per_mtok: Some(prices.input),
        price_out_per_mtok: Some(prices.output),
        executor: None,
    };
    let guard = EgressGuard { local_only: false, consented_host: Some(host) };
    let provider = build_provider(&profile, Some(Secret::new(key)), guard).expect("provider");
    let ledger = Path::new(&out).join("hard_runs.jsonl");
    let mut spent = 0.0;
    let mut worst = 0.0f64;
    for task in &list {
        let id = task["id"].as_str().unwrap().to_string();
        if !only.is_empty() && !only.contains(&id) {
            continue;
        }
        let cap = per_task.min(budget - spent);
        if spent + worst.min(per_task) >= budget || cap <= 0.0 {
            eprintln!("budget {budget} reached (spent {spent:.3}) before {id}");
            break;
        }
        let expect = &task["expect"];
        let mut ed = fresh_editor();
        // the HVAC and question tasks work on an existing project; `base: "@id"` starts from
        // the project an earlier task saved, `base: path` from that file
        let start = match task["base"].as_str() {
            Some(b) => Some(match b.strip_prefix('@') {
                Some(prev) => Path::new(&out).join(format!("{prev}.ddc")).to_string_lossy().into_owned(),
                None => b.to_string(),
            }),
            None => task["kind"].is_string().then(|| base.clone()),
        };
        if let Some(src) = start {
            if !Path::new(&src).exists() {
                let line = json!({"phase": "hard", "id": id, "ok": false, "error": format!("base {src} missing")});
                let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&ledger).unwrap();
                writeln!(f, "{line}").unwrap();
                eprintln!("{id}: base {src} missing, skipped");
                continue;
            }
            let copy = Path::new(&out).join(format!("{id}_base.ddc"));
            std::fs::copy(&src, &copy).unwrap();
            ed.open_project(&copy).unwrap();
        }
        for (k, v) in task["policy"].as_object().into_iter().flatten() {
            ed.model.set_ai_policy(k, v.as_str().unwrap_or_default()).unwrap();
        }
        let before = summary(&ed);
        let mut c = cfg(ddc_ai::fake::ScriptedProvider::new(vec![]));
        let trace = Trace::new(provider.clone());
        c.provider = trace.clone();
        c.profile = profile.clone();
        // the run stands in for the person: images are allowed unless the scenario policy says no
        c.allow_snapshots = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            ed.model.ai_policy("egress.snapshots").ok().flatten().as_deref() != Some("0"),
        ));
        c.limits = Limits {
            max_steps: 40,
            max_tool_calls: 120,
            max_turn_tokens: 8_000_000,
            // backstop only: the loop prices every input token at the full rate
            max_cost: Some(cap * 10.0),
            max_wall_s: 1500,
            ..Limits::default()
        };
        let (cr0, cw0) = ddc_ai::cache_totals();
        let t = Instant::now();
        let view = view_of(&ed);
        let mut s = UiSession::start(&mut ed, c, view).unwrap();
        assert!(wait(&mut s, &mut ed, WAIT), "{id}: the session did not open");
        // the brief, then each of `turns` as a follow-up once the previous turn is idle
        let msgs: Vec<String> = std::iter::once(task["brief"].as_str().unwrap().to_string())
            .chain(task["turns"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(str::to_string)))
            .collect();
        let (mut text, mut tools, mut errors, mut usage, mut failed) = (String::new(), vec![], vec![], None, false);
        // how each turn ended: answered, or which limit cut it
        let mut stops: Vec<String> = vec![];
        let (mut last, mut stopped) = (0.0f64, false);
        let mut finished = true;
        for msg in msgs {
            if stopped || failed || !finished {
                break;
            }
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            let view = view_of(&ed);
            s.send_turn(&mut ed, msg, view, Mode::Propose).unwrap();
            let deadline = Instant::now() + Duration::from_secs(1600);
        // the budget guard: real cost after every step; cancel when one more step like the last
        // would cross the task cap or the run budget
        finished = loop {
            let idle = wait(&mut s, &mut ed, Duration::from_secs(1));
            for e in s.drain_events() {
                match e {
                    UiEvent::Text(x) => text.push_str(&x),
                    UiEvent::Tool { name, ok: Some(false), err, .. } => {
                        errors.push(format!("{name}: {}", err.unwrap_or_default()));
                        tools.push(name);
                    }
                    UiEvent::Tool { name, ok: Some(true), .. } => tools.push(name),
                    UiEvent::Usage(u) => {
                        let (cr, cw) = ddc_ai::cache_totals();
                        let now = prices.cost(u.session.input_tokens, u.session.output_tokens, cr - cr0, cw - cw0);
                        let step = now - last;
                        last = now;
                        if !stopped && (spent + now + step >= budget || now + step >= cap) {
                            s.cancel();
                            stopped = true;
                        }
                        usage = Some(u);
                    }
                    UiEvent::TurnDone(end) => stops.push(format!("{end:?}")),
                    UiEvent::Failed(e) => {
                        stops.push(format!("failed: {e:?}"));
                        failed = true;
                        errors.push(format!("failed: {e}"))
                    }
                    _ => {}
                }
            }
            if idle || Instant::now() > deadline {
                break idle;
            }
        };
        }
        let ms = t.elapsed().as_millis() as u64;
        let drafted = s.plan().map_or(0, |p| p.items.len());
        let apply = expect["model"]["apply"].as_bool().unwrap_or(true);
        let applied = match s.plan() {
            Some(p) if apply && !p.items.is_empty() => {
                if !p.unreviewed.is_empty() || s.redraft().is_some() {
                    let rev = s.plan_rev();
                    s.review(Some(rev));
                    wait(&mut s, &mut ed, WAIT);
                }
                if !s.can_apply() {
                    s.allow_large(true);
                    wait(&mut s, &mut ed, WAIT);
                }
                match s.apply(&mut ed) {
                    Ok(_) => wait(&mut s, &mut ed, WAIT),
                    Err(e) => {
                        errors.push(format!("apply: {e}"));
                        false
                    }
                }
            }
            _ => false,
        };
        let (cr1, cw1) = ddc_ai::cache_totals();
        let (cr, cw) = (cr1 - cr0, cw1 - cw0);
        let u = usage.map(|u| u.session).unwrap_or_default();
        let fresh = u.input_tokens.saturating_sub(cr + cw);
        let cost = prices.cost(u.input_tokens, u.output_tokens, cr, cw);
        spent += cost;
        worst = worst.max(cost);
        let file = Path::new(&out).join(format!("{id}.ddc"));
        let _ = std::fs::rename(&file, Path::new(&out).join(format!("{id}.prev.ddc")));
        let saved = ed.save_project_as(&file).is_ok();
        let after = summary(&ed);
        let projects = stored_projects(&ed);
        let checks = live_score::score_model(expect, finished && !failed, applied, drafted, &text, &before, &after);
        let (passed, of) = live_score::tally(&checks);
        let line = json!({
            "phase": "hard", "id": id, "kind": task["kind"], "model": model, "finished": finished,
            "applied": applied, "drafted": drafted, "saved": saved, "tool_calls": tools, "tool_errors": errors,
            "input_tokens": u.input_tokens, "fresh_input_tokens": fresh, "output_tokens": u.output_tokens,
            "cache_read": cr, "cache_write": cw, "cost_usd": (cost * 10000.0).round() / 10000.0,
            "latency_ms": ms, "budget_stop": stopped, "ok": passed == of, "checks": checks, "score": [passed, of],
            "model_before": before, "model_after": after, "model_text": text, "projects": projects,
            "stops": stops, "trace": trace.rows(),
        });
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&ledger).unwrap();
        writeln!(f, "{line}").unwrap();
        eprintln!("{id}: checks={passed}/{of} applied={applied} tools={} errors={} ${cost:.3} {ms}ms", tools.len(), errors.len());
        drop(s);
        if stopped && spent + worst >= budget {
            eprintln!("budget {budget} reached inside {id}, stopping");
            break;
        }
    }
    eprintln!("spent ${spent:.3}");
    let lines: Vec<J> = std::fs::read_to_string(&ledger)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let labels: J = serde_json::from_str(LABELS).unwrap();
    std::fs::write(Path::new(&out).join("hard_report.html"), live_score::report_html(&lines, &labels)).unwrap();
}
