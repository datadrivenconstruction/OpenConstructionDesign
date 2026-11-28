// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Evaluation corpus of AI-written families: 30 briefs, the recorded answers of a model
//! (several attempts where the first one was refused), replayed through the same repair loop the
//! assistant uses. A task counts as ok when a draft is accepted and every `must` holds on the
//! parsed family. Hard briefs come first. No network: `AIFAM_EVAL_LEDGER=1` appends a ledger row.

#[path = "e2e_ai_common/mod.rs"]
#[allow(dead_code)]
mod e2e_ai_common;
#[allow(dead_code)]
mod e2e_common;
#[path = "../../ai/examples/common/live_score.rs"]
#[allow(dead_code)]
mod live_score;

use ddc_ai::family_repair::RepairState;
use ddc_geom::component::{Family, Lod};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ai/data/evals/family")
}

#[derive(Debug)]
struct Task {
    id: String,
    brief: String,
    musts: Vec<String>,
    attempts: Vec<String>,
    /// The category a correct answer has.
    category: String,
    /// `outcome = "refused"`: the recorded answers are refused as the model wrote them; the task
    /// counts as not ok and the recording is never edited to pass.
    refused: bool,
}

fn load() -> Vec<Task> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let id = p.file_stem().unwrap().to_string_lossy().to_string();
            let src = std::fs::read_to_string(&p).unwrap();
            let (mut brief, mut musts, mut category, mut refused) = (String::new(), vec![], String::new(), false);
            for l in src.lines() {
                let Some((k, v)) = l.split_once(" = ") else { continue };
                match k {
                    "brief" => brief = v.trim_matches('"').to_string(),
                    "must" => musts.push(v.trim().to_string()),
                    "category" => category = v.trim().trim_matches('"').to_string(),
                    "outcome" => match v.trim().trim_matches('"') {
                        "refused" => refused = true,
                        o => panic!("{}: unknown outcome '{o}'", p.display()),
                    },
                    _ => {}
                }
            }
            let mut attempts = vec![];
            for n in 1.. {
                let f = root().join("recorded").join(format!("{id}.{n}.fam"));
                match std::fs::read_to_string(f) {
                    Ok(t) => attempts.push(t),
                    Err(_) => break,
                }
            }
            Task { id, brief, musts, attempts, category, refused }
        })
        .collect()
}

fn built(f: &Family, extra: &str) -> Result<ddc_geom::component::Built, String> {
    let mut tv = f.types.first().map(|t| t.values.clone()).unwrap_or_default();
    let mut inst = vec![];
    if !extra.is_empty() {
        // a type parameter flexes through its type, an instance parameter on the instance
        for (k, v) in f.assignments(extra)? {
            if f.param(&k).is_some_and(|p| !p.instance) {
                tv.retain(|(n, _)| *n != k);
                tv.push((k, v));
            } else {
                inst.push((k, v));
            }
        }
    }
    let r = f.resolve(&tv, &inst).map_err(|e| e.to_string())?;
    f.build(&r, Lod::Medium).map_err(|e| e.to_string())
}

/// One `must` line against the parsed family; `Err` says what does not hold.
fn check(f: &Family, must: &str) -> Result<(), String> {
    let w: Vec<&str> = must.split_whitespace().collect();
    let num = |s: &str| s.parse::<f64>().map_err(|_| format!("bad number '{s}' in '{must}'"));
    let want = |ok: bool, what: String| if ok { Ok(()) } else { Err(format!("{must}: {what}")) };
    match w.as_slice() {
        ["param", n] => want(f.param(n).is_some(), "no such parameter".into()),
        ["ports", n] => {
            let n = num(n)? as usize;
            want(f.connectors.len() == n, format!("has {}", f.connectors.len()))
        }
        ["port", d, n] => {
            let n = num(n)? as usize;
            let have = f.connectors.iter().filter(|c| c.domain.name() == *d).count();
            want(have == n, format!("has {have}"))
        }
        ["nests", n] => {
            let n = num(n)? as usize;
            want(f.ext.nests.len() == n, format!("has {}", f.ext.nests.len()))
        }
        ["solids", ">=", n] => {
            let n = num(n)? as usize;
            want(f.solids.len() >= n, format!("has {}", f.solids.len()))
        }
        ["visible", body] => want(f.ext.visible.iter().any(|(b, _)| b == body), "no rule".into()),
        ["bbox", axis, mm] => {
            let b = built(f, "")?;
            let e = match *axis {
                "x" => b.bbox[1].x - b.bbox[0].x,
                "y" => b.bbox[1].y - b.bbox[0].y,
                _ => b.bbox[1].z - b.bbox[0].z,
            } * 1000.0;
            let t = num(mm)?;
            want((e - t).abs() <= t * 0.02, format!("is {e:.0} mm"))
        }
        ["flex", p, lo, hi] => {
            for v in [lo, hi] {
                built(f, &format!("{p}={v}mm")).map_err(|e| format!("{must}: at {v}: {e}"))?;
            }
            Ok(())
        }
        _ => Err(format!("unknown check '{must}'")),
    }
}

struct Outcome {
    id: String,
    accepted: bool,
    ok: bool,
    first: bool,
    rounds: u32,
    tokens: u64,
    ms: u128,
    why: String,
}

fn run(t: &Task) -> Outcome {
    let start = Instant::now();
    let mut st = RepairState::default();
    let (mut accepted, mut first, mut why) = (None, false, String::from("never accepted"));
    // an accepted draft resets the loop state, so the rounds are read after each refusal
    let (mut rounds, mut tokens) = (0, 0);
    for (i, a) in t.attempts.iter().enumerate() {
        match st.draft(&json!({ "text": a }), &|_| None) {
            Ok(d) => {
                first = i == 0;
                accepted = Some(d);
                break;
            }
            Err(w) => {
                why = w.chars().take(160).collect();
                (rounds, tokens) = (st.rounds, st.tokens);
            }
        }
    }
    let ms = start.elapsed().as_millis();
    let mut ok = false;
    let was_accepted = accepted.is_some();
    if let Some(d) = accepted {
        match Family::parse(&d.text) {
            Err(e) => why = format!("accepted text does not parse: {e}"),
            Ok(f) => {
                let bad: Vec<String> = t.musts.iter().filter_map(|m| check(&f, m).err()).collect();
                ok = bad.is_empty();
                why = bad.join("; ");
            }
        }
    }
    Outcome { id: t.id.clone(), accepted: was_accepted, ok, first: ok && first, rounds, tokens, ms, why }
}

#[test]
fn eval_corpus_parses() {
    let tasks = load();
    assert_eq!(tasks.len(), 30);
    for t in &tasks {
        assert!(!t.brief.is_empty(), "{}", t.id);
        assert!(t.musts.len() >= 3, "{}: {} checks", t.id, t.musts.len());
        assert!(!t.attempts.is_empty(), "{}: no recorded answer", t.id);
        // every check is a known kind: it is evaluated against its own recording; a recording
        // kept refused is read with the expected category only for this syntax check
        let last = t.attempts.last().unwrap();
        let text = if t.refused {
            let e = Family::parse(last).expect_err(&format!("{}: recorded as refused, but it parses", t.id));
            assert!(e.to_string().contains("category"), "{}: refused for another reason: {e}", t.id);
            assert!(!t.category.is_empty(), "{}: a refused task names the expected category", t.id);
            last.lines()
                .map(|l| if l.starts_with("category ") { format!("category {}", t.category) } else { l.to_string() })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            last.clone()
        };
        let f = Family::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", t.id));
        for m in &t.musts {
            assert!(!check(&f, m).is_err_and(|e| e.contains("unknown check") || e.contains("bad number")), "{}: {m}", t.id);
        }
    }
    // the hard briefs come first, the plain table and chair last
    let pos = |s: &str| tasks.iter().position(|t| t.id.contains(s)).unwrap();
    assert!(pos("lattice") < pos("storage_rack") && pos("pump_group") < pos("table"));
    assert!(tasks.iter().take(5).all(|t| t.attempts.len() == 2), "hard ones are recorded with a refused first try");
}

#[test]
fn must_checks_are_semantic() {
    let t = load().into_iter().find(|t| t.id.contains("ahu_complex")).unwrap();
    let good = t.attempts.last().unwrap().clone();
    let f = Family::parse(&good).unwrap();
    for m in &t.musts {
        check(&f, m).unwrap_or_else(|e| panic!("{e}"));
    }
    // renaming a body and adding a comment changes the text, not the meaning
    let renamed = good.replace("extrude Casing", "extrude Shell # the box").replace("connector Power", "connector Mains");
    let f2 = Family::parse(&renamed).unwrap();
    for m in &t.musts {
        check(&f2, m).unwrap_or_else(|e| panic!("{e}"));
    }
    // dropping a connection or shrinking the box does
    let fewer = good.replace("connector Power conduit power at(-Length / 2, 0, Height / 2) dir(-1, 0, 0) size(32mm)\n", "");
    assert!(check(&Family::parse(&fewer).unwrap(), "ports 6").is_err());
    let small = good.replace("param Length length type 2400mm", "param Length length type 2000mm").replace("Length=2400mm", "Length=2000mm");
    assert!(check(&Family::parse(&small).unwrap(), "bbox x 2400").is_err());
}

#[test]
fn recorded_run_meets_the_bar() {
    let start = Instant::now();
    let tasks = load();
    let out: Vec<Outcome> = tasks.iter().map(run).collect();
    let n = out.len() as f64;
    let ok = out.iter().filter(|o| o.ok).count() as f64 / n * 100.0;
    let first = out.iter().filter(|o| o.first).count() as f64 / n * 100.0;
    let steps = out.iter().map(|o| o.rounds as f64).sum::<f64>() / n;
    let tokens = out.iter().map(|o| o.tokens as f64).sum::<f64>() / n;
    let ms = out.iter().map(|o| o.ms).sum::<u128>();
    println!("{:<28} {:>3} {:>5} {:>6} {:>7} {:>6}", "task", "ok", "first", "rounds", "tokens", "ms");
    for o in &out {
        println!("{:<28} {:>3} {:>5} {:>6} {:>7} {:>6} {}", o.id, o.ok, o.first, o.rounds, o.tokens, o.ms, o.why);
    }
    println!("ok {ok:.0}%  first-time {first:.0}%  steps {steps:.2}  tokens {tokens:.0}  validate {ms} ms");
    // a recording kept refused counts against the ok share, must really be refused, and is not
    // listed as a regression
    for (t, o) in tasks.iter().zip(&out).filter(|(t, _)| t.refused) {
        assert!(!o.accepted && !o.ok && o.rounds >= 1, "{}: recorded as refused, repair rounds {}", t.id, o.rounds);
    }
    let bad: Vec<String> = tasks
        .iter()
        .zip(&out)
        .filter(|(t, o)| !t.refused && !o.ok)
        .map(|(_, o)| format!("{}: {}", o.id, o.why))
        .collect();
    assert!(start.elapsed().as_secs() <= 60, "the recorded run takes {:?}", start.elapsed());

    let ledger = root().join("../ledger.jsonl");
    let prev = std::fs::read_to_string(&ledger)
        .ok()
        .and_then(|s| s.lines().rev().find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok().filter(|v| v["kind"] == "family_recorded")))
        .and_then(|v| v["ok_pct"].as_f64());
    if std::env::var("AIFAM_EVAL_LEDGER").is_ok() {
        use std::io::Write;
        let row = json!({ "kind": "family_recorded", "model": "recorded", "tasks": out.len(),
            "ok_pct": ok, "first_pct": first, "avg_steps": steps, "avg_tokens": tokens, "validate_ms": ms });
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&ledger).unwrap();
        writeln!(f, "{row}").unwrap();
    }
    assert!(bad.is_empty(), "tasks not ok: {bad:?}");
    if let Some(p) = prev {
        assert!(ok >= p - 5.0, "ok share fell from {p:.0}% to {ok:.0}%");
    }
    assert!(out.iter().take(5).all(|o| o.rounds >= 1), "hard tasks exercise the repair loop");
}

/// Live run of the same briefs against a real provider (manual: `check.ps1 aifam-eval -Live`).
/// DDC_LIVE_PROFILE=profile.json DDC_LIVE_OUT=dir DDC_AI_KEY=...; DDC_LIVE_ONLY=id,id picks tasks,
/// otherwise the five hard ones run; DDC_LIVE_ALL=1 runs all 30. Each task is one chat turn; the
/// family the assistant drafted is scored with the same `must` checks as the recorded run.
#[test]
#[ignore = "needs a provider profile and a key"]
fn live_family_tasks() {
    use ddc_ai::{build_provider, AuthKind, EgressGuard, Limits, Mode, Profile, Protocol, Secret, ToolCalling};
    use e2e_ai_common::{cfg, start, turn};
    let (Ok(pf), Ok(out)) = (std::env::var("DDC_LIVE_PROFILE"), std::env::var("DDC_LIVE_OUT")) else {
        eprintln!("set DDC_LIVE_PROFILE and DDC_LIVE_OUT");
        return;
    };
    let pf = live_score::load_profile(&pf).unwrap();
    let Some(key) = live_score::read_key(&["DDC_AI_KEY"]) else {
        eprintln!("no key: set DDC_AI_KEY");
        return;
    };
    let prices = live_score::Prices::of(&pf["prices"]);
    let model = std::env::var("DDC_LIVE_MODEL").unwrap_or_else(|_| pf["model"].as_str().unwrap().to_string());
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
        images: false,
        context_tokens: 200_000,
        max_output_tokens: 8192,
        price_in_per_mtok: Some(prices.input),
        price_out_per_mtok: Some(prices.output),
        executor: None,
        reach: None,
    };
    let guard = EgressGuard { local_only: false, consented_host: Some(host) };
    let provider = build_provider(&profile, Some(Secret::new(key)), guard).expect("provider");
    let only: Vec<String> = std::env::var("DDC_LIVE_ONLY")
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let all = std::env::var("DDC_LIVE_ALL").is_ok();
    let tasks: Vec<Task> = load()
        .into_iter()
        .enumerate()
        .filter(|(i, t)| if only.is_empty() { all || *i < 5 } else { only.iter().any(|o| t.id.contains(o)) })
        .map(|x| x.1)
        .collect();
    std::fs::create_dir_all(&out).unwrap();
    let mut rows: Vec<(String, bool, bool, u32, u64)> = vec![];
    for t in &tasks {
        let mut ed = e2e_ai_common_editor();
        let mut c = cfg(ddc_ai::fake::ScriptedProvider::new(vec![]));
        c.provider = provider.clone();
        c.profile = profile.clone();
        c.limits = Limits { max_steps: 24, max_tool_calls: 48, max_turn_tokens: 300_000, ..Limits::default() };
        let mut s = start(&mut ed, c);
        turn(&mut s, &mut ed, &format!("Write a family definition and offer it with family_draft. {}", t.brief), Mode::Propose);
        let drafts: Vec<(String, (u32, u64))> = s
            .drain_events()
            .into_iter()
            .filter_map(|e| match e {
                ddc_app::ai::session::UiEvent::FamilyDraft { text, repair, .. } => Some((text, repair)),
                _ => None,
            })
            .collect();
        let (ok, first, rounds, tokens) = match drafts.last() {
            Some((text, repair)) => {
                let ok = Family::parse(text).map(|f| t.musts.iter().all(|m| check(&f, m).is_ok())).unwrap_or(false);
                (ok, ok && repair.0 == 0, repair.0, repair.1)
            }
            None => (false, false, 0, 0),
        };
        eprintln!("{:<28} ok {ok} first {first} rounds {rounds} tokens {tokens}", t.id);
        rows.push((t.id.clone(), ok, first, rounds, tokens));
    }
    let n = rows.len().max(1) as f64;
    let ok = rows.iter().filter(|r| r.1).count() as f64 / n * 100.0;
    let first = rows.iter().filter(|r| r.2).count() as f64 / n * 100.0;
    let row = json!({ "kind": "family_live", "model": model, "tasks": rows.len(), "ok_pct": ok, "first_pct": first,
        "avg_steps": rows.iter().map(|r| r.3 as f64).sum::<f64>() / n,
        "avg_tokens": rows.iter().map(|r| r.4 as f64).sum::<f64>() / n });
    eprintln!("{row}");
    use std::io::Write;
    let ledger = root().join("../ledger.jsonl");
    let prev = std::fs::read_to_string(&ledger).ok().and_then(|s| {
        s.lines()
            .rev()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["kind"] == "family_live" && v["model"] == row["model"])
            .and_then(|v| v["ok_pct"].as_f64())
    });
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&ledger).unwrap();
    writeln!(f, "{row}").unwrap();
    if let Some(p) = prev {
        assert!(ok >= p - 5.0, "live ok share fell from {p:.0}% to {ok:.0}%");
    }
}

fn e2e_ai_common_editor() -> ddc_app::editor::Editor {
    let mut ed = ddc_app::editor::Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}
