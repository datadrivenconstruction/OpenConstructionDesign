// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Real-API measurement of the building generator: drives the app's conversation loop with a
//! host that only knows generate_project / revise_project, and scores every run objectively.
//!
//! DDC_AI_KEY=... cargo run -p ddc-ai --example gen_matrix -- --profile P.json --out DIR
//! Flags: --phase NAME  --only id,id  --levels L1,H  --budget USD  --model ID  --tasks FILE
//!        --key-env NAME (fallback key variable)  --report LEDGER[,LEDGER] --html FILE (no API)
//! The profile file holds endpoint, model, auth header and prices (see common/live_score.rs).
//! Tasks with a `kind` (HVAC, questions) need the app's tools and go to ddc-app e2e_ai_live.
#[path = "common/live_score.rs"]
mod live_score;

use ddc_ai::{catalog, toolset};
use ddc_ai::{
    build_provider, AuthKind, CallCx, CancelToken, Conversation, ConvoEvent, EgressGuard, Limits,
    Mode, Profile, Protocol, Secret, ToolCalling, ToolHost, ToolOutcome, J,
};
use ddc_gen::project::{self, merge_patch, GProject};
use live_score::Prices;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use std::time::Instant;

const LABELS: &str = include_str!("common/report_labels.json");

struct Host {
    spec: Option<J>,
    built: Option<GProject>,
    typology: String,
    gates_ok: bool,
    gate_failures: Vec<String>,
    collisions: Vec<String>,
    calls: Vec<String>,
}

/// Layout errors that are physical clashes, not missing data.
fn is_clash(e: &str) -> bool {
    ["overlap", "cross", "meets", "outside", "does not fit", "taller than"].iter().any(|k| e.contains(k))
}

impl Host {
    fn build(&mut self, spec: J) -> ToolOutcome {
        self.spec = Some(spec.clone());
        let s = match project::parse_spec(&spec) {
            Ok(s) => s,
            Err(e) => return ToolOutcome::error(e.to_string()),
        };
        let p = match project::build_project(&s) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::error(e.to_string()),
        };
        let g = project::check_project(&s, &p);
        self.collisions = p
            .storeys
            .iter()
            .enumerate()
            .filter_map(|(k, st)| ddc_gen::validate_layout(&st.layout).err().map(|e| format!("floor {}: {e}", k + 1)))
            .filter(|e| is_clash(e))
            .collect();
        let metrics: serde_json::Map<String, J> = p
            .metrics
            .iter()
            .map(|m| (m.0.to_string(), json!((m.1 * 100.0).round() / 100.0)))
            .collect();
        let counts: serde_json::Map<String, J> =
            g.counts.iter().map(|c| (c.0.to_string(), json!(c.1))).collect();
        let brief = project::brief::check(&s, &p).report;
        self.built = Some(p);
        self.typology = s.typology_name().to_string();
        self.gates_ok = g.passed();
        self.gate_failures = g.failures.clone();
        if !g.passed() {
            return ToolOutcome::error(format!(
                "the project fails its gates: {}",
                g.failures.join("; ")
            ));
        }
        let w: Vec<&String> = g.warnings.iter().take(10).collect();
        ToolOutcome::ok(
            json!({"typology": s.typology_name(), "brief_check": brief, "metrics": metrics,
                   "counts": counts, "gates": "passed", "warnings": w,
                   "more_warnings": g.warnings.len().saturating_sub(w.len())})
            .to_string(),
        )
    }
}

impl ToolHost for Host {
    fn call(&mut self, name: &str, args: &J, _cx: &CallCx<'_>) -> ToolOutcome {
        self.calls.push(name.to_string());
        match name {
            "generate_project" => self.build(args["spec"].clone()),
            "revise_project" => {
                let Some(mut spec) = self.spec.clone() else {
                    return ToolOutcome::error("no project yet: call generate_project");
                };
                if !args["changes"].is_object() {
                    return ToolOutcome::error("changes must be an object");
                }
                merge_patch(&mut spec, &args["changes"]);
                self.build(spec)
            }
            _ => ToolOutcome::error("not available in this harness: only generate_project and revise_project run"),
        }
    }
}

fn est(t: &ddc_ai::ToolSchema) -> u64 {
    ((t.name.len() + t.description.len() + t.input_schema.to_string().len()) / 4) as u64
}

fn ring_area(v: &[(f64, f64)]) -> f64 {
    let n = v.len();
    (0..n).map(|i| v[i].0 * v[(i + 1) % n].1 - v[(i + 1) % n].0 * v[i].1).sum::<f64>().abs() / 2.0
}

fn counts_of(p: &GProject) -> J {
    let (mut walls, mut doors, mut windows, mut rooms, mut stairs, mut cols) = (0, 0, 0, 0, 0, 0);
    let mut fix: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &p.storeys {
        let l = &s.layout;
        walls += l.walls.len();
        rooms += l.rooms.len();
        stairs += l.stairs.len();
        cols += l.columns.len();
        for o in &l.openings {
            if o.door {
                doors += 1
            } else {
                windows += 1
            }
        }
        for f in &l.fixtures {
            *fix.entry(f.family).or_default() += 1;
        }
    }
    json!({"storeys": p.storeys.len(), "walls": walls, "doors": doors, "windows": windows,
           "rooms": rooms, "slabs": p.storeys.len() + 1, "stairs": stairs, "columns": cols,
           "fixtures": fix})
}

/// What the scorer compares with the brief: storeys, slab area net of holes, rooms per kind.
fn facts_of(h: &Host, dropped: &[String]) -> J {
    let Some(p) = &h.built else {
        return json!({"built": false, "dropped_features": dropped});
    };
    let gfa: f64 = p
        .storeys
        .iter()
        .map(|s| ring_area(&s.layout.slab) - s.layout.slab_holes.iter().map(|r| ring_area(r)).sum::<f64>())
        .sum();
    let mut rooms: BTreeMap<String, usize> = BTreeMap::new();
    for s in &p.storeys {
        for r in &s.rooms {
            *rooms.entry(format!("{:?}", r.kind).to_lowercase()).or_default() += 1;
        }
    }
    json!({"built": true, "gate_failures": h.gate_failures,
           "collisions": h.collisions, "storeys": p.storeys.len(), "gfa": (gfa * 10.0).round() / 10.0,
           "footprint": [p.footprint.0, p.footprint.1], "rooms": rooms, "apartments": p.apartments,
           "typology": h.typology, "dropped_features": dropped})
}

fn dump_geometry(p: &GProject) -> J {
    let pts = |v: &[(f64, f64)]| v.iter().map(|q| json!([q.0, q.1])).collect::<Vec<_>>();
    let storeys: Vec<J> = p
        .storeys
        .iter()
        .map(|s| {
            let l = &s.layout;
            json!({
                "name": s.name, "elevation": s.elevation, "height": s.height,
                "walls": l.walls.iter().map(|w| json!({"a": [w.a.0, w.a.1], "b": [w.b.0, w.b.1],
                    "bulge": w.bulge, "role": format!("{:?}", w.role), "t": w.role.thickness()})).collect::<Vec<_>>(),
                "openings": l.openings.iter().map(|o| json!({"wall": o.wall, "along": o.along,
                    "door": o.door, "w": o.width, "h": o.height, "sill": o.sill})).collect::<Vec<_>>(),
                "slab": pts(&l.slab),
                "slab_holes": l.slab_holes.iter().map(|h| pts(h)).collect::<Vec<_>>(),
                "rooms": l.rooms.iter().map(|r| json!({"name": r.name, "poly": pts(&r.polygon)})).collect::<Vec<_>>(),
                "stairs": l.stairs.iter().map(|t| json!({"at": [t.origin.0, t.origin.1], "angle": t.angle, "w": t.width})).collect::<Vec<_>>(),
                "columns": l.columns.iter().map(|c| json!({"at": [c.at.0, c.at.1], "s": [c.section.0, c.section.1]})).collect::<Vec<_>>(),
                "fixtures": l.fixtures.iter().map(|f| json!({"family": f.family, "at": [f.at.0, f.at.1], "rot": f.rotation})).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({"storeys": storeys, "roof": {"elevation": p.roof.elevation, "ring": pts(&p.roof.ring),
        "slope": p.roof.slope, "overhang": p.roof.overhang}, "footprint": [p.footprint.0, p.footprint.1]})
}

/// Keywords that would show a feature in the spec text; stairs/lift/multi_floor use the output.
fn feature_present(f: &str, spec: &str, p: Option<&GProject>) -> bool {
    let has = |ks: &[&str]| ks.iter().any(|k| spec.contains(k));
    match f {
        "stairs" => p.is_some_and(|p| p.storeys.iter().any(|s| !s.layout.stairs.is_empty())),
        "lift" => spec.contains("\"lift\":true"),
        "multi_floor" => p.is_some_and(|p| p.storeys.len() > 1),
        "pitched_roof" | "sloped_roof" => p.is_some_and(|p| p.roof.slope > 3.0),
        "l_shape" => has(&["l_shape", "l-shape", "l shape", "polygon", "\"massing\":\"l\""]),
        "u_shape" => has(&["u_shape", "u-shape", "u shape", "polygon", "\"massing\":\"u\""]),
        "courtyard" => has(&["courtyard", "polygon"]),
        "balcony" => has(&["balcon", "loggia"]),
        "curtain_wall" => has(&["curtain"]),
        "setback" => has(&["setback", "step"]),
        "cantilever" => has(&["cantilever", "overhang"]),
        "atrium" => has(&["atrium"]),
        "curved" => has(&["curve", "arc", "freeform"]),
        "mep" => has(&["mep", "duct", "pipe", "hvac", "sprinkler"]),
        _ => false,
    }
}

fn arg(a: &[String], k: &str) -> Option<String> {
    a.iter().position(|x| x == k).and_then(|i| a.get(i + 1).cloned())
}

fn list(a: &[String], k: &str) -> Vec<String> {
    arg(a, k).map(|s| s.split(',').map(String::from).collect()).unwrap_or_default()
}

fn read_ledger(path: &str) -> Vec<J> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn write_report(ledgers: &[String], html: &str) {
    let lines: Vec<J> = ledgers.iter().flat_map(|l| read_ledger(l)).collect();
    let labels: J = serde_json::from_str(LABELS).unwrap();
    std::fs::write(html, live_score::report_html(&lines, &labels)).unwrap();
    eprintln!("report: {html} ({} runs)", lines.len());
}

/// Matrix prompts as they are; task-file entries ({id, kind?, brief, checklist, expect}) mapped
/// onto the same fields. The level of a task is its letter: H, V or Q.
fn load_prompts(tasks: Option<String>) -> Vec<J> {
    match tasks {
        Some(f) => {
            let t: Vec<J> = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
            t.iter()
                .map(|x| {
                    let id = x["id"].as_str().unwrap();
                    json!({"id": id, "type": "task", "kind": x["kind"], "level": &id[..1], "typology": "",
                           "prompt": x["brief"], "features": [], "checklist": x["checklist"], "expect": x["expect"]})
                })
                .collect()
        }
        None => serde_json::from_str(include_str!("gen_matrix_prompts.json")).unwrap(),
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let out = arg(&a, "--out").unwrap_or_else(|| "target/gen_matrix".into());
    std::fs::create_dir_all(format!("{out}/matrix")).unwrap();
    let ledger_path = format!("{out}/token_ledger.jsonl");
    if a.iter().any(|x| x == "--report") {
        let html = arg(&a, "--html").unwrap_or_else(|| format!("{out}/report.html"));
        write_report(&list(&a, "--report"), &html);
        return;
    }
    let phase = arg(&a, "--phase").unwrap_or_else(|| "after".into());
    let only = list(&a, "--only");
    let levels = list(&a, "--levels");
    let Some(budget) = arg(&a, "--budget").and_then(|s| s.parse::<f64>().ok()) else {
        eprintln!("--budget USD is required");
        std::process::exit(2);
    };
    let Some(pf) = arg(&a, "--profile") else {
        eprintln!("--profile FILE is required: base_url, model, auth_header, prices");
        std::process::exit(2);
    };
    let pf = live_score::load_profile(&pf).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    });
    let prices = Prices::of(&pf["prices"]);
    let model = arg(&a, "--model").unwrap_or_else(|| pf["model"].as_str().unwrap().to_string());
    let fallback = arg(&a, "--key-env");
    let mut names = vec!["DDC_AI_KEY"];
    if let Some(f) = fallback.as_deref() {
        names.push(f);
    }
    let Some(key) = live_score::read_key(&names) else {
        eprintln!("no key: set DDC_AI_KEY (or name another variable with --key-env)");
        std::process::exit(2);
    };
    let prompts = load_prompts(arg(&a, "--tasks"));

    // what the first request carries: the chat core (groups join only when loaded)
    let tools = toolset::chat_schemas(Mode::Propose, true, &Default::default());
    let total: u64 = tools.iter().map(est).sum();
    let catalog_total: u64 = catalog::schemas(Mode::Propose, true).iter().map(est).sum();
    let gp = tools.iter().find(|t| t.name == "generate_project").map(est).unwrap_or(0);
    let mut man: Vec<(u64, String)> = tools.iter().map(|t| (est(t), t.name.clone())).collect();
    man.sort_by(|x, y| y.cmp(x));
    let _ = std::fs::write(
        format!("{out}/tools_manifest.json"),
        json!({"count": tools.len(), "tokens_est": total, "catalog_tokens_est": catalog_total,
               "generate_project_est": gp,
               "tools": man.iter().map(|(n, s)| json!({"name": s, "tokens_est": n})).collect::<Vec<_>>()})
        .to_string(),
    );
    eprintln!("tools sent: {} (~{total} tokens est), generate_project ~{gp}", tools.len());

    let headers: Vec<(String, String)> = pf["headers"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
        .collect();
    let base_url = pf["base_url"].as_str().unwrap().to_string();
    let host_name = base_url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("").to_string();
    let profile = Profile {
        id: "matrix".into(),
        name: "matrix".into(),
        protocol: Protocol::ContentBlocks,
        base_url,
        model: model.clone(),
        auth: AuthKind::Header { name: pf["auth_header"].as_str().unwrap().to_string() },
        extra_headers: headers,
        tool_calling: ToolCalling::Native,
        images: true,
        context_tokens: 200_000,
        max_output_tokens: 8192,
        price_in_per_mtok: Some(prices.input),
        price_out_per_mtok: Some(prices.output),
        executor: None,
        reach: None,
    };
    let guard = EgressGuard { local_only: false, consented_host: Some(host_name) };
    let provider = build_provider(&profile, Some(Secret::new(key)), guard).expect("provider");

    let mut spent = 0.0;
    let mut worst = 0.0f64;
    for pr in &prompts {
        let id = pr["id"].as_str().unwrap();
        let lvl = pr["level"].as_str().unwrap();
        if !only.is_empty() && !only.iter().any(|o| o == id) {
            continue;
        }
        if !levels.is_empty() && !levels.iter().any(|l| l == lvl) {
            continue;
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&ledger_path).unwrap();
        if pr["kind"].is_string() {
            let line = json!({"phase": phase, "id": id, "level": lvl, "kind": pr["kind"],
                              "skipped": "needs the app tools: run ddc-app e2e_ai_live"});
            writeln!(f, "{line}").unwrap();
            continue;
        }
        // the dearest run so far is the reserve: a run that might not fit is not started
        if spent + worst >= budget {
            eprintln!("budget {budget} reached (spent {spent:.4}, reserve {worst:.4}), stopping before {id}");
            break;
        }
        // backstop only: the loop prices every input token at the full rate; the guard is the
        // cancel in the usage callback below
        let limits = Limits { max_steps: 4, max_cost: Some((budget - spent) * 10.0), ..Limits::default() };
        let cancel = CancelToken::new();
        let mut conv = Conversation::new(provider.clone(), profile.clone(), limits, cancel.clone());
        let mut host = Host {
            spec: None,
            built: None,
            typology: String::new(),
            gates_ok: false,
            gate_failures: vec![],
            collisions: vec![],
            calls: vec![],
        };
        let mut text = String::new();
        let mut sent = 0u32;
        let mut tool_err: Vec<String> = vec![];
        let mut stopped = false;
        let (cr0, cw0) = ddc_ai::cache_totals();
        let mut last = 0.0f64;
        let t = Instant::now();
        let res = conv.run_turn(
            pr["prompt"].as_str().unwrap().to_string(),
            String::new(),
            Mode::Propose,
            &mut host,
            &mut |e| match e {
                ConvoEvent::Text(s) => text.push_str(&s),
                ConvoEvent::Sent(_) => sent += 1,
                ConvoEvent::ToolEnd { name, is_error: true, detail } => {
                    tool_err.push(format!("{name}: {}", detail.unwrap_or_default()))
                }
                ConvoEvent::Usage(u) => {
                    // real cost so far; stop when one more step like the last would cross the budget
                    let (cr, cw) = ddc_ai::cache_totals();
                    let s = u.session;
                    let now = prices.cost(s.input_tokens, s.output_tokens, cr - cr0, cw - cw0);
                    let step = now - last;
                    last = now;
                    if spent + now + step >= budget && !cancel.is_cancelled() {
                        cancel.cancel();
                        stopped = true;
                    }
                }
                _ => {}
            },
        );
        let ms = t.elapsed().as_millis() as u64;
        let (cr1, cw1) = ddc_ai::cache_totals();
        let (cache_read, cache_write) = (cr1 - cr0, cw1 - cw0);
        let u = conv.session_usage();
        let fresh = u.input_tokens.saturating_sub(cache_read + cache_write);
        let cost = prices.cost(u.input_tokens, u.output_tokens, cache_read, cache_write);
        spent += cost;
        worst = worst.max(cost);
        let spec_text = host.spec.as_ref().map(|s| s.to_string().to_lowercase()).unwrap_or_default();
        let lower_text = text.to_lowercase();
        let feats: Vec<String> = pr["features"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect();
        let dropped: Vec<String> = feats
            .iter()
            .filter(|f| !feature_present(f, &spec_text, host.built.as_ref()))
            .cloned()
            .collect();
        let acknowledged: Vec<&String> = dropped
            .iter()
            .filter(|f| {
                let k = f.split(['_', ':']).next().unwrap_or("");
                !k.is_empty() && lower_text.contains(k)
            })
            .collect();
        let facts = facts_of(&host, &dropped);
        let checks = live_score::score_generate(&pr["expect"], &facts, &text);
        let (passed, of) = live_score::tally(&checks);
        let built_ok = res.is_ok() && host.built.is_some() && host.gates_ok;
        let err = match &res {
            Err(e) => Some(format!("{e:?}")),
            Ok(_) if !built_ok => Some(tool_err.last().cloned().unwrap_or_else(|| "no project built".into())),
            _ => None,
        };
        let line = json!({
            "phase": phase, "type": pr["type"], "level": lvl, "id": id, "model": model,
            "steps": sent, "tool_calls": host.calls, "tool_errors": tool_err,
            "tools_sent": tools.len(), "tools_tokens_est": total, "generate_project_tokens_est": gp,
            "input_tokens": u.input_tokens, "fresh_input_tokens": fresh, "output_tokens": u.output_tokens,
            "cache_read": cache_read, "cache_write": cache_write, "cost_usd": (cost * 10000.0).round() / 10000.0,
            "latency_ms": ms, "spec": host.spec, "built": built_ok, "ok": passed == of, "error": err,
            "budget_stop": stopped, "checks": checks, "score": [passed, of], "facts": facts,
            "counts": host.built.as_ref().map(counts_of),
            "requested_features": feats, "dropped_features": dropped, "acknowledged_in_text": acknowledged,
            "model_text": text.chars().take(600).collect::<String>(),
        });
        writeln!(f, "{line}").unwrap();
        if let Some(p) = &host.built {
            let _ = std::fs::write(format!("{out}/matrix/{id}.json"), dump_geometry(p).to_string());
        }
        eprintln!("{id}: built={built_ok} checks={passed}/{of} in={} out={} ${cost:.4} {ms}ms", u.input_tokens, u.output_tokens);
        if stopped {
            eprintln!("budget {budget} reached inside {id}, stopping");
            break;
        }
    }
    eprintln!("spent ${spent:.4}");
    write_report(&[ledger_path], &format!("{out}/report.html"));
}
