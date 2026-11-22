// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Reference scenarios of the geometry plan (g12): recorded tool-call scripts run through the
//! fake provider, measured and, when `DDC_BENCH_RECORD=1`, appended to
//! the AI geometry bench ledger. Gates here: 0 live writes before Apply, one undo step,
//! schedule counts equal the model.

use crate::{e2e_ai_common, e2e_common};

use ddc_ai::fake::{RecordedRequest, Script, ScriptedProvider};
use ddc_ai::provider::{Part, Role};
use ddc_ai::Mode;
use ddc_app::ai::attach::{self, AttachData};
use ddc_app::editor::Editor;
use ddc_app::schedule::{build_in, Cat};
use ddc_model::checks::{CheckRule, CheckScope, Finding, Fix, Severity};
use ddc_model::{Error, MemberShape, Model};
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::{json, Value as J};
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

/// Reference prices, USD per million tokens (a mid-size hosted model); the provider has none.
const PRICE_IN: f64 = 3.0;
const PRICE_OUT: f64 = 15.0;
/// Tables whose rows are the model's elements.
const ELEMENTS: &[&str] = &[
    "wall", "opening", "slab", "roof", "ceiling", "col", "beam", "stair", "room", "comp",
    "solid", "mep_run", "curtain_wall",
];

/// Scenarios that wait for another module: name and the reason written to the ledger.
const BLOCKED: &[(&str, &str)] = &[
];

#[derive(Default)]
struct Metrics {
    scenario: String,
    draft_ms: u128,
    apply_ms: u128,
    spend: Spend,
    elements: i64,
    plan_items: usize,
    check_findings: usize,
    critic_findings: Option<usize>,
    manual_edits: u32,
    live_writes_before_apply: i64,
    undo_steps: i64,
    applied: bool,
}

/// The realism critic plugs in here; until its module exists the metric is null.
fn critic_findings(_ed: &Editor) -> Option<usize> {
    None
}

fn tokens(chars: usize) -> u64 {
    (chars as u64).div_ceil(4)
}

/// What a scenario's requests cost, split the way the wire is: tools, system, history.
#[derive(Default)]
struct Spend {
    requests: usize,
    /// Tokens of the tool catalog and of the system text in the last request.
    tools: u64,
    system: u64,
    /// Input over all requests, and the part of it read from cache.
    tokens_in: u64,
    tokens_cached: u64,
    tokens_out: u64,
    cost: f64,
    cost_cached: f64,
    /// Where the input went, summed over all requests (`BUCKETS` order).
    by: [u64; BUCKETS.len()],
}

/// Parts of a request, in wire order after the tools and the system text.
const BUCKETS: [&str; 6] = ["tools", "system", "user", "calls", "results", "images"];
/// What an image costs as input (the conversation's own estimate).
const IMAGE_CHARS: usize = 1500 * 4;

/// The request as the wire orders it: tool catalog, system text, the rest of the messages,
/// written the way the wire writes them (JSON arguments, plain text), plus the size of each
/// bucket in characters.
fn request_text(r: &RecordedRequest) -> (String, String, String, [usize; BUCKETS.len()]) {
    let tools: String =
        r.tools.iter().map(|t| format!("{}{}{}", t.name, t.description, t.input_schema)).collect();
    let (mut system, mut rest) = (String::new(), String::new());
    let mut by = [0usize; BUCKETS.len()];
    for m in &r.messages {
        for p in &m.parts {
            let (i, text) = match p {
                Part::Text(t) if matches!(m.role, Role::System) => (1, t.clone()),
                Part::Text(t) if matches!(m.role, Role::User) => (2, t.clone()),
                Part::Text(t) => (3, t.clone()),
                Part::ToolCall { id, name, args } => (3, format!("{id}{name}{args}")),
                Part::ToolResult { id, content, images, .. } => {
                    by[5] += images.len() * IMAGE_CHARS;
                    (4, format!("{id}{content}"))
                }
                Part::Image(_) => {
                    by[5] += IMAGE_CHARS;
                    continue;
                }
            };
            by[i] += text.len();
            if i == 1 {
                system.push_str(&text);
            } else {
                rest.push_str(&text);
            }
        }
    }
    by[0] = tools.len();
    // images are not in the text; they still count once per request (no cache in this model)
    rest.push_str(&"#".repeat(by[5]));
    (tools, system, rest, by)
}

/// Token estimate from the recorded requests (no tokenizer here: 4 chars a token). A request
/// is read from cache up to the longest prefix it shares with the one before; the rest is fresh.
fn spend(p: &ScriptedProvider, script_chars: usize) -> Spend {
    let mut s = Spend::default();
    let mut prev = String::new();
    for r in p.requests() {
        let (tools, system, rest, by) = request_text(&r);
        let all = format!("{tools}{system}{rest}");
        let t = tokens(all.len());
        let shared = all.bytes().zip(prev.bytes()).take_while(|(a, b)| a == b).count();
        let cached = tokens(shared).min(t);
        s.requests += 1;
        s.tools = tokens(tools.len());
        s.system = tokens(system.len());
        for (acc, n) in s.by.iter_mut().zip(by) {
            *acc += tokens(n);
        }
        s.tokens_in += t;
        s.tokens_cached += cached;
        s.cost += t as f64 * PRICE_IN;
        s.cost_cached += (t - cached) as f64 * PRICE_IN + cached as f64 * PRICE_IN * 0.1;
        prev = all;
    }
    s.tokens_out = tokens(script_chars);
    let o = s.tokens_out as f64 * PRICE_OUT;
    s.cost = (s.cost + o) / 1e6;
    s.cost_cached = (s.cost_cached + o) / 1e6;
    s
}

fn script_chars(tools: &[Script]) -> usize {
    tools
        .iter()
        .map(|t| match t {
            Script::Tool { name, args } => name.len() + args.to_string().len(),
            _ => 0,
        })
        .sum()
}

fn element_count(ed: &Editor) -> i64 {
    ELEMENTS.iter().map(|t| count(&ed.model, t)).sum()
}

fn check_findings(ed: &Editor) -> usize {
    ddc_model::checks::run_all(&ed.model, None, &CheckScope::All, 100_000)
        .map(|f| f.len())
        .unwrap_or(0)
}

fn ledger_path() -> std::path::PathBuf {
    // DDC_BENCH_LEDGER names the file; the default stays inside the build output.
    std::env::var_os("DDC_BENCH_LEDGER").map_or_else(
        || std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench/ai_geometry.jsonl"),
        std::path::PathBuf::from,
    )
}

/// One JSON line per run; written only when asked, so a plain test run leaves the tree clean.
fn record(line: &J) {
    println!("BENCH {line}");
    if std::env::var_os("DDC_BENCH_RECORD").is_none() {
        return;
    }
    let path = ledger_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

/// Names the state of the product a ledger line was measured on (`DDC_BENCH_VARIANT`).
fn variant() -> String {
    std::env::var("DDC_BENCH_VARIANT").unwrap_or_else(|_| "current".to_string())
}

fn ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Metrics {
    fn line(&self) -> J {
        let by: serde_json::Map<String, J> =
            BUCKETS.iter().zip(self.spend.by).map(|(k, v)| (k.to_string(), json!(v))).collect();
        json!({
            "ts": ts(), "scenario": self.scenario, "status": "ok",
            "applied": self.applied,
            "draft_ms": self.draft_ms, "apply_ms": self.apply_ms,
            "variant": variant(), "requests": self.spend.requests,
            "tokens_in": self.spend.tokens_in, "tokens_cached": self.spend.tokens_cached,
            "tokens_fresh": self.spend.tokens_in - self.spend.tokens_cached,
            "tokens_out": self.spend.tokens_out,
            "tokens_tools": self.spend.tools, "tokens_system": self.spend.system,
            "tokens_by": by,
            "cost_usd": round(self.spend.cost), "cost_cached_usd": round(self.spend.cost_cached),
            "elements": self.elements, "plan_items": self.plan_items,
            "check_findings": self.check_findings, "critic_findings": self.critic_findings,
            "manual_edits": self.manual_edits,
            "live_writes_before_apply": self.live_writes_before_apply,
            "undo_steps": self.undo_steps,
        })
    }
}

/// Per-scenario budget of the replay: input tokens over all requests and the cost with the
/// cache (plan g12: a gazebo at most 0.10 USD). Lowered as the product gets cheaper, never raised.
const BUDGETS: &[(&str, u64, f64)] = &[
    ("gazebos_4", 40_000, 0.10),
    ("gazebos_4_one_by_one", 400_000, 0.40),
    ("rural_toilet", 60_000, 0.20),
    ("audit_custom_rules", 30_000, 0.10),
];

impl Metrics {
    /// The line is printed first, so a miss shows where the tokens went.
    fn within_budget(&self) {
        let Some(&(_, max_in, max_cost)) = BUDGETS.iter().find(|b| b.0 == self.scenario) else {
            panic!("{}: no token budget", self.scenario);
        };
        let s = &self.spend;
        assert!(s.tokens_in <= max_in, "{}: {} input tokens > {max_in}", self.scenario, s.tokens_in);
        assert!(
            s.cost_cached <= max_cost,
            "{}: {:.4} USD > {max_cost}",
            self.scenario,
            s.cost_cached
        );
    }
}

fn round(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

/// One call a reply: what a model does when each call needs the previous result.
fn one_by_one(tools: Vec<Script>) -> Vec<Vec<Script>> {
    tools.into_iter().map(|t| vec![t]).collect()
}

/// Run a recorded script (one inner list is one model reply) to the draft, then Apply it
/// (checked: one undo, exact restore).
fn scenario(name: &str, ed: &mut Editor, replies: Vec<Vec<Script>>) -> Metrics {
    scenario_with(name, ed, replies, vec![])
}

/// `scenario` with files attached to the chat before the turn.
fn scenario_with(name: &str, ed: &mut Editor, replies: Vec<Vec<Script>>, files: Vec<Arc<AttachData>>) -> Metrics {
    let chars: usize = replies.iter().map(|r| script_chars(r)).sum();
    let before = live_hash(&ed.model);
    let ops = count(&ed.model, "op");
    let t0 = Instant::now();
    let mut turns = replies;
    turns.push(vec![Script::Text("done".to_string())]);
    let provider = ScriptedProvider::new(turns);
    let mut s = start(ed, cfg(provider.clone()));
    if !files.is_empty() {
        s.set_attachments(files, vec![]);
    }
    turn(&mut s, ed, "go", Mode::Propose);
    let draft_ms = t0.elapsed().as_millis();
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{name}: {results:?}");
    let wrote = (live_hash(&ed.model) != before) as i64 + (count(&ed.model, "op") - ops);
    let plan_items = plan_of(&s).items.len();

    let t1 = Instant::now();
    apply_and_check(&mut s, ed);
    let apply_ms = t1.elapsed().as_millis();
    let spend = spend(&provider, chars);
    let m = Metrics {
        scenario: name.to_string(),
        draft_ms,
        apply_ms,
        spend,
        elements: element_count(ed),
        plan_items,
        check_findings: check_findings(ed),
        critic_findings: critic_findings(ed),
        manual_edits: 0,
        live_writes_before_apply: wrote,
        undo_steps: 1,
        applied: true,
    };
    record(&m.line());
    assert_eq!(m.live_writes_before_apply, 0, "{name}: 0 writes before Apply");
    m.within_budget();
    m
}

/// A solid extruded `l` metres along `dir` (0 x, 1 y, 2 z) from `at`, written the short way
/// the tool description teaches (bare metres, `[x, y, z]`, the frame only when not upright,
/// the level once per call): what the model is billed for as output and resent as input.
fn bar(profile: J, at: (f64, f64, f64), dir: usize, l: f64) -> J {
    let origin = json!([at.0, at.1, at.2]);
    let form = match dir {
        2 => json!({"profile": profile, "origin": origin, "height": l}),
        d => {
            let (z, x) = if d == 0 { ([1, 0, 0], [0, 1, 0]) } else { ([0, 1, 0], [1, 0, 0]) };
            json!({"profile": profile, "frame": {"origin": origin, "x": x, "z": z}, "height": l})
        }
    };
    json!({"action": "solid", "form": form})
}

fn rect(w: f64, h: f64) -> J {
    json!({"shape": "rect", "w": w, "h": h})
}

fn round_bar(r: f64) -> J {
    json!({"shape": "circle", "r": r})
}

fn steel_h() -> J {
    json!({"shape": "h", "h": 0.15, "b": 0.15, "tw": 0.008, "tf": 0.01})
}

fn steel_i() -> J {
    json!({"shape": "i", "h": 0.2, "b": 0.1, "tw": 0.006, "tf": 0.009})
}

const SIDE: f64 = 4.0;
const H: f64 = 2.6;

fn corners(x0: f64) -> [(f64, f64); 4] {
    [(x0, 0.0), (x0 + SIDE, 0.0), (x0 + SIDE, SIDE), (x0, SIDE)]
}

/// Four posts and four beams round the top, from the given section builders.
fn frame_ops(x0: f64, post: J, beam: J, post_top: f64) -> Vec<J> {
    let mut ops: Vec<J> = corners(x0)
        .iter()
        .map(|c| bar(post.clone(), (c.0, c.1, 0.0), 2, post_top))
        .collect();
    for dy in [0.0, SIDE] {
        ops.push(bar(beam.clone(), (x0, dy, post_top), 0, SIDE));
    }
    for dx in [0.0, SIDE] {
        ops.push(bar(beam.clone(), (x0 + dx, 0.0, post_top), 1, SIDE));
    }
    ops
}

fn geometry(title: &str, ops: Vec<J>, lvl: &J) -> Script {
    tool("create_geometry", json!({"title": title, "level": lvl, "ops": ops}))
}

fn ring(x0: f64, grow: f64) -> Vec<J> {
    corners(x0)
        .iter()
        .zip([(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)])
        .map(|(c, d)| json!([c.0 + d.0 * grow, c.1 + d.1 * grow]))
        .collect()
}

/// Floor and roof as elements, so every gazebo has schedulable rows.
fn deck_and_roof(title: &str, x0: f64) -> Script {
    tool(
        "create_elements",
        json!({"title": title, "cat": "floor", "items": [
            {"outer": ring(x0, 0.0)}]}),
    )
}

fn roof(title: &str, x0: f64) -> Script {
    tool(
        "create_elements",
        json!({"title": title, "cat": "roof", "items": [
            {"ring": ring(x0, 0.4), "offset": H + 0.2, "slope": 20}]}),
    )
}

fn schedule(cat: &str) -> Script {
    tool("create_schedule", json!({"title": cat, "cat": cat, "columns": ["id"]}))
}

fn timber(x0: f64) -> Vec<Script> {
    let posts: Vec<J> = corners(x0)
        .iter()
        .map(|c| json!({"at": [c.0, c.1], "top_offset": H}))
        .collect();
    let beams: Vec<J> = (0..4)
        .map(|i| {
            let (a, b) = (corners(x0)[i], corners(x0)[(i + 1) % 4]);
            json!({"a": [a.0, a.1, H], "b": [b.0, b.1, H]})
        })
        .collect();
    vec![
        tool("create_elements", json!({"title": "Timber posts", "cat": "column", "items": posts})),
        tool("create_elements", json!({"title": "Timber beams", "cat": "beam", "items": beams})),
        deck_and_roof("Timber deck", x0),
        roof("Timber roof", x0),
    ]
}

fn steel(x0: f64, lvl: &J) -> Vec<Script> {
    vec![
        geometry("Steel frame", frame_ops(x0, steel_h(), steel_i(), H), lvl),
        deck_and_roof("Steel deck", x0),
        roof("Steel roof", x0),
    ]
}

fn log(x0: f64, lvl: &J) -> Vec<Script> {
    let mut ops = frame_ops(x0, round_bar(0.12), round_bar(0.1), H);
    for dy in [1.0, 2.0, 3.0] {
        ops.push(bar(round_bar(0.06), (x0, dy, H + 0.12), 0, SIDE));
    }
    vec![geometry("Log frame", ops, lvl), deck_and_roof("Log deck", x0), roof("Log roof", x0)]
}

/// Footings below grade, posts, a ring beam and four rebar bars in each post.
fn concrete(x0: f64, lvl: &J) -> Vec<Script> {
    let mut ops: Vec<J> = corners(x0)
        .iter()
        .map(|c| bar(rect(0.8, 0.8), (c.0, c.1, -0.5), 2, 0.5))
        .collect();
    ops.extend(frame_ops(x0, rect(0.3, 0.3), rect(0.25, 0.25), H));
    for c in corners(x0) {
        for (dx, dy) in [(-0.1, -0.1), (0.1, -0.1), (0.1, 0.1), (-0.1, 0.1)] {
            ops.push(bar(round_bar(0.008), (c.0 + dx, c.1 + dy, -0.4), 2, H + 0.3));
        }
    }
    vec![geometry("Concrete frame", ops, lvl), deck_and_roof("Concrete deck", x0), roof("Roof", x0)]
}

fn gazebos(lvl: &J) -> Vec<Script> {
    let mut all = vec![];
    all.extend(timber(0.0));
    all.extend(steel(8.0, lvl));
    all.extend(log(16.0, lvl));
    all.extend(concrete(24.0, lvl));
    // the sheets: the schedules the person prints
    for cat in ["column", "beam", "floor", "roof"] {
        all.push(schedule(cat));
    }
    all
}

/// The four gazebos are independent of each other, so a model may send them in one reply or,
/// as an older one did, one call at a time; both ways must give the same model.
fn gazebos_scenario(name: &str, batched: bool) {
    let mut ed = Editor::new_in_memory().unwrap();
    // the project's own type library: the elements take their types from it
    let post = MemberShape::Rect { b: 0.2, h: 0.2 };
    ed.model.add_member_type("Timber 200", &post, "Timber").unwrap();
    ed.model.add_slab_type("Deck 150", &e2e_ai_common::concrete(0.15)).unwrap();
    ed.reload().unwrap();
    let lvl = json!({"by": "id", "cat": "level", "id": ed.level});
    let calls = gazebos(&lvl);
    let replies = if batched { vec![calls] } else { one_by_one(calls) };
    let m = scenario(name, &mut ed, replies);
    // schedule counts equal the model
    let rows = |c| build_in(&ed, c, false).rows.len();
    assert_eq!(rows(Cat::Columns), 4, "timber posts");
    assert_eq!(rows(Cat::Beams), 4, "timber beams");
    assert_eq!(rows(Cat::Slabs), 4, "a deck each");
    assert_eq!(rows(Cat::Roofs), 4, "a roof each");
    assert_eq!(count(&ed.model, "solid"), 8 + 11 + 28, "steel 8, log 11, concrete 28");
    assert!(m.elements >= 4 * 2 + 4 + 4 + 4 * 8, "{}", m.elements);
}

#[test]
fn four_gazebos_one_draft_one_apply_one_undo() {
    gazebos_scenario("gazebos_4", true);
}

#[test]
fn four_gazebos_one_call_per_reply_give_the_same_model() {
    gazebos_scenario("gazebos_4_one_by_one", false);
}

fn toilet() -> J {
    let len = |n: &str, d: i64| json!({"name": n, "type": "length", "scope": "type",
        "unit": "mm", "default": d});
    json!({
        "name": "RuralToilet", "category": "generic", "host": "none",
        "params": [len("Length", 1200), len("Depth", 1200), len("Height", 2200),
                   len("Overhang", 150), len("Wall", 100), len("HoleD", 300)],
        "forms": [
            {"name": "Shell", "extrude": {"rect": ["0", "0", "Length", "Depth"]},
             "z": ["0", "Height"]},
            {"name": "Cavity", "void": true,
             "extrude": {"rect": ["Wall", "Wall", "Length - Wall", "Depth - Wall"]},
             "z": ["Wall", "Height"]},
            {"name": "SeatHole", "void": true,
             "extrude": {"circle": ["Length / 2", "Depth / 2", "HoleD / 2"]}, "z": ["0", "Wall"]},
            {"name": "Roof",
             "extrude": {"rect": ["-Overhang", "-Overhang", "Length + Overhang",
                                  "Depth + Overhang"]},
             "z": ["Height", "Height + 100mm"]}
        ],
        "tests": [{"Length": 900}, {"Depth": 2000, "Overhang": 300}]
    })
}

#[test]
fn rural_toilet_family_six_params_three_instances() {
    let mut ed = Editor::new_in_memory().unwrap();
    let tools = vec![
        tool("define_family", json!({"title": "Rural toilet", "family": toilet()})),
        tool("create_type", json!({"title": "Type", "cat": "comp", "name": "Std",
            "fields": {"family": "RuralToilet"}})),
        tool("create_elements", json!({"title": "Three toilets", "cat": "comp", "items": [
            {"type": "comp_type:n1", "at": [0, 0, 0]},
            {"type": "comp_type:n1", "at": [4, 0, 0]},
            {"type": "comp_type:n1", "at": [8, 0, 0], "rotation": 90}]})),
    ];
    // each call names what the previous one made, so these cannot share a reply
    let m = scenario("rural_toilet", &mut ed, one_by_one(tools));
    assert_eq!(count(&ed.model, "comp"), 3);
    assert_eq!(m.plan_items, 3);
}

/// Doors narrower than the company's 0.8 m: the audit of scenario 6.
struct DoorWidth;

impl CheckRule for DoorWidth {
    fn id(&self) -> &'static str {
        "company.door_width"
    }
    fn run(&self, m: &Model, _: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let mut st = m.sql().prepare("SELECT id FROM opening WHERE kind='door' AND width<0.8")?;
        for id in st.query_map([], |r| r.get::<_, i64>(0))? {
            out.push(Finding {
                rule: "company.door_width",
                severity: Severity::Error,
                elements: vec![("opening".to_string(), id?)],
                at: None,
                measure: None,
                msg_key: "company.narrow",
                args: vec![],
                fix: Fix::None,
            });
        }
        Ok(())
    }
}

static DOOR_WIDTH: DoorWidth = DoorWidth;

#[test]
fn audit_by_custom_rules_reports_and_writes_nothing() {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let wall = add_wall(&mut ed, level, (0.0, 0.0), (40.0, 0.0));
    let mut narrow = 0;
    for i in 0..12 {
        let d = add_door(&mut ed, wall, 1.5 + 3.0 * i as f64);
        if i % 3 == 0 {
            let spec = ddc_model::OpeningSpec {
                along: 1.5 + 3.0 * i as f64,
                width: 0.65,
                sill: 0.0,
                height: 2.1,
                kind: "door".to_string(),
            };
            ed.model.update_opening(d, &spec).unwrap();
            narrow += 1;
        }
    }
    ed.reload().unwrap();
    let tools = vec![tool("run_checks", json!({"rules": ["company.door_width"]}))];
    let chars = script_chars(&tools);
    let (before, ops) = (live_hash(&ed.model), count(&ed.model, "op"));
    let provider = ScriptedProvider::new(script(tools));
    let mut s = start(&mut ed, with_rules(cfg(provider.clone()), vec![&DOOR_WIDTH]));
    let t0 = Instant::now();
    turn(&mut s, &mut ed, "audit the doors", Mode::Propose);
    let draft_ms = t0.elapsed().as_millis();
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert_eq!(parsed(&results[0].0)["total"], json!(narrow));
    let wrote = (live_hash(&ed.model) != before) as i64 + (count(&ed.model, "op") - ops);
    assert_eq!(wrote, 0, "an audit writes nothing");
    assert!(s.plan().is_none(), "an audit leaves no plan");
    let spend = spend(&provider, chars);
    let m = Metrics {
        scenario: "audit_custom_rules".to_string(),
        draft_ms,
        spend,
        elements: element_count(&ed),
        check_findings: narrow as usize,
        critic_findings: critic_findings(&ed),
        ..Metrics::default()
    };
    record(&m.line());
    m.within_budget();
}

/// Plan g9: an 8 x 6 m house drawn as a PNG (filled walls, swings, glass lines) is attached;
/// the model reads it with plan_from_image, names the two rooms it sees, and the draft is applied.
#[test]
fn house_from_plan_image() {
    use ddc_io::plan_image::synth::{SynthOpening, SynthPlan, SynthWall, WallStyle};
    let w = |a: (f64, f64), b: (f64, f64), t: f64| SynthWall { a, b, thickness: t };
    let o = |wall: usize, along: f64, width: f64, door: bool| SynthOpening { wall, along, width, door };
    let house = SynthPlan {
        walls: vec![
            w((0.0, 0.0), (8.0, 0.0), 0.3),
            w((8.0, 0.0), (8.0, 6.0), 0.3),
            w((8.0, 6.0), (0.0, 6.0), 0.3),
            w((0.0, 6.0), (0.0, 0.0), 0.3),
            w((4.0, 0.0), (4.0, 6.0), 0.12),
        ],
        openings: vec![o(0, 1.0, 1.5, false), o(2, 1.5, 1.2, false), o(4, 2.0, 0.9, true), o(1, 2.5, 1.0, true)],
    };
    let k = 50.0;
    let (img, shift) = house.render(k, 1.0, WallStyle::Filled);
    let px = |x: f64, y: f64| json!([(x + shift.0) * k, img.h as f64 - (y + shift.1) * k]);
    let file = attach::from_bytes("plan.png", img.to_png().unwrap(), 9).unwrap();

    let mut ed = Editor::new_in_memory().unwrap();
    ed.model.add_wall_type("Outer 300", &e2e_ai_common::concrete(0.3)).unwrap();
    ed.model.add_wall_type("Inner 120", &e2e_ai_common::concrete(0.12)).unwrap();
    ed.reload().unwrap();
    let call = tool("plan_from_image", json!({
        "title": "House from the plan picture", "id": 9, "px_per_m": k,
        "offset": [-shift.0, -shift.1],
        "rooms": [{"name": "Living", "number": "1", "at": px(2.0, 3.0)},
                  {"name": "Bedroom", "number": "2", "at": px(6.0, 3.0)}],
    }));
    let m = scenario_with("house_from_plan_image", &mut ed, vec![vec![call]], vec![Arc::new(file)]);
    assert_eq!(count(&ed.model, "opening"), 4, "2 doors and 2 windows");
    assert_eq!(count(&ed.model, "room"), 2);
    let q = |sql: &str| -> f64 { ed.model.sql().query_row(sql, [], |r| r.get(0)).unwrap() };
    assert!(q("SELECT min(min(ax, bx)) FROM wall").abs() < 0.03);
    assert!((q("SELECT max(max(ax, bx)) FROM wall") - 8.0).abs() < 0.03);
    assert!((q("SELECT max(max(ay, by)) FROM wall") - 6.0).abs() < 0.03);
    // wall axes: 2 x 8 + 3 x 6 m, the partition may stop at the faces it meets
    let total = q("SELECT sum(sqrt((bx-ax)*(bx-ax) + (by-ay)*(by-ay))) FROM wall");
    assert!((total - 34.0).abs() <= 34.0 * 0.05, "{total}");
    assert_eq!(q("SELECT count(*) FROM opening WHERE kind='door'"), 2.0);
    assert!(m.plan_items >= 4, "walls, doors, windows, rooms: {}", m.plan_items);
}

#[test]
fn scenarios_waiting_for_other_modules_are_declared_with_a_reason() {
    for (name, reason) in BLOCKED {
        let (tool_word, _) = reason.split_once(' ').unwrap();
        assert!(tool_word.starts_with('g'), "{name}: {reason}");
        record(&json!({"ts": ts(), "scenario": name, "status": "skipped", "reason": reason}));
    }
    // g2 and g7 are real runs below: the sphere needs run_script, the floor needs supply_fixtures
    assert!(ddc_ai::catalog::find("run_script").is_some(), "g2 scenario needs run_script");
    assert!(ddc_ai::catalog::find("supply_fixtures").is_some(), "g7 scenario needs supply_fixtures");
}

const SPHERE: &str = include_str!("../../../script/data/steel_sphere.lua");
const SPHERE_GATE_S: f64 = 10.0;

/// g2: a steel sphere of about 4500 parts from one script, built and applied, then the radius
/// changed through the stored recipe: each step within ten seconds, nothing written before Apply.
#[test]
#[cfg_attr(debug_assertions, ignore = "timing gate: run with --release")]
fn steel_sphere_builds_and_rebuilds_within_ten_seconds() {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = add_level(&mut ed, "L1", 0.0);
    add_wall(&mut ed, level, (0.0, 5.0), (10.0, 5.0));
    let before = live_hash(&ed.model);
    let ops = count(&ed.model, "op");
    let t0 = Instant::now();
    let (mut s, provider) = run(
        &mut ed,
        vec![tool("run_script", json!({"title": "Steel sphere", "script": SPHERE, "params": {}}))],
    );
    let draft_ms = t0.elapsed().as_millis();
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let first = parsed(&results[0].0);
    let parts = first["changes"]["created"].as_u64().unwrap();
    let wrote = (live_hash(&ed.model) != before) as i64 + (count(&ed.model, "op") - ops);
    if !s.can_apply() {
        s.allow_large(true);
        assert!(s.wait_idle(&mut ed, WAIT));
    }
    let t1 = Instant::now();
    s.apply(&mut ed).unwrap();
    assert!(s.wait_idle(&mut ed, WAIT));
    let apply_ms = t1.elapsed().as_millis();
    let solids = count(&ed.model, "solid") as u64;
    drop(s);

    let recipe = ed.model.recipes().unwrap()[0].id;
    let t2 = Instant::now();
    let (mut s, provider) = run(
        &mut ed,
        vec![tool(
            "rerun_recipe",
            json!({"title": "Radius 14 m", "recipe": recipe, "params": {"radius": "14 m"}}),
        )],
    );
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    if !s.can_apply() {
        s.allow_large(true);
        assert!(s.wait_idle(&mut ed, WAIT));
    }
    s.apply(&mut ed).unwrap();
    assert!(s.wait_idle(&mut ed, WAIT));
    let rerun_ms = t2.elapsed().as_millis();
    let build_s = (draft_ms + apply_ms) as f64 / 1000.0;
    let rerun_s = rerun_ms as f64 / 1000.0;
    record(&json!({
        "ts": ts(), "scenario": "steel_sphere", "gate": "g2", "status": "ok",
        "variant": variant(), "parts": parts, "solids": solids,
        "draft_ms": draft_ms, "apply_ms": apply_ms, "build_s": round(build_s),
        "rerun_ms": rerun_ms, "gate_s": SPHERE_GATE_S,
        "live_writes_before_apply": wrote, "script_cpu_ms": first["stats"]["cpu_ms"],
    }));
    assert_eq!(wrote, 0, "0 writes before Apply");
    assert_eq!(solids, parts);
    assert!((4300..=4700).contains(&parts), "{parts}");
    assert!(build_s <= SPHERE_GATE_S, "build and apply {build_s} s");
    assert!(rerun_s <= SPHERE_GATE_S, "radius change {rerun_s} s");
}

const BATHROOMS: usize = 12;
const BATH_CELL: f64 = 3.0;
const MEP_GATE_S: f64 = 30.0;

/// A library fixture of a bathroom: free-standing near its wall, or on the wall when the family
/// is wall-hosted (`along` is the distance from the wall's start).
fn bath_fixture(ed: &mut Editor, wall: i64, x0: f64, along: f64, family: &str, ty: &str, z: f64) {
    let m = &mut ed.model;
    let f = m.load_library_family(family).unwrap();
    let ct = m.comp_type_id(f, ty).unwrap().unwrap();
    let mut spec = ddc_model::CompSpec::at(ed.level, ct, x0 + along, 1.0);
    spec.at.2 = z;
    match m.add_component(&spec) {
        Ok(_) => return,
        Err(e) if e.to_string().contains("wall-hosted") => {}
        Err(e) => panic!("{family}: {e}"),
    }
    spec.host = ddc_model::CompHost::Wall(wall);
    spec.at = (along, 0.1, z);
    m.add_component(&spec).unwrap();
}

/// A residential floor: twelve bathrooms in a row, each with a WC and a washbasin.
fn bathroom_floor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    for k in 0..BATHROOMS {
        let x0 = k as f64 * BATH_CELL;
        let wall = add_wall(&mut ed, level, (x0, 0.0), (x0 + BATH_CELL, 0.0));
        bath_fixture(&mut ed, wall, x0, 0.8, "WC", "Floor 650", 0.0);
        bath_fixture(&mut ed, wall, x0, 2.0, "Washbasin", "500", 0.8);
    }
    ed
}

/// g7: water supply and drainage for twelve bathrooms of a floor from one intent (a riser),
/// one draft: within 30 s, 0 clashes, no connector left unrouted, falls in the norm.
#[test]
#[cfg_attr(debug_assertions, ignore = "timing gate: run with --release")]
fn floor_mep_12_bathrooms_routes_water_and_drains_in_thirty_seconds() {
    let mut ed = bathroom_floor();
    let before = live_hash(&ed.model);
    let ops = count(&ed.model, "op");
    let t0 = Instant::now();
    let (mut s, provider) = run(
        &mut ed,
        vec![tool(
            "supply_fixtures",
            json!({"title": "Water and drains, 12 bathrooms", "riser": [18.0, 2.5]}),
        )],
    );
    let draft_ms = t0.elapsed().as_millis();
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let metrics = parsed(&results[0].0)["metrics"].clone();
    let wrote = (live_hash(&ed.model) != before) as i64 + (count(&ed.model, "op") - ops);
    let t1 = Instant::now();
    apply_and_check(&mut s, &mut ed);
    let apply_ms = t1.elapsed().as_millis();
    let total_s = (draft_ms + apply_ms) as f64 / 1000.0;
    record(&json!({
        "ts": ts(), "scenario": "floor_mep_12_bathrooms", "gate": "g7", "status": "ok",
        "variant": variant(), "bathrooms": BATHROOMS, "draft_ms": draft_ms,
        "apply_ms": apply_ms, "total_s": round(total_s), "gate_s": MEP_GATE_S,
        "runs": metrics["runs"], "length_m": metrics["length_m"], "clashes": metrics["clashes"],
        "failed": metrics["failed"], "min_slope_pct": metrics["min_slope_pct"],
        "live_writes_before_apply": wrote,
    }));
    assert_eq!(wrote, 0, "0 writes before Apply");
    assert_eq!(metrics["clashes"], json!(0), "clashes with the structure: {metrics}");
    assert_eq!(metrics["failed"], json!([]), "connectors left unrouted: {metrics}");
    if let Some(slope) = metrics["min_slope_pct"].as_f64() {
        assert!(slope >= 2.0 - 1e-6, "a drain falls less than 2 %: {metrics}");
    }
    assert!(total_s <= MEP_GATE_S, "water and drains {total_s} s");
    let f = assert_services(&ed.model, true);
    assert_eq!(f.wet, 2 * BATHROOMS, "{f:?}");
}
