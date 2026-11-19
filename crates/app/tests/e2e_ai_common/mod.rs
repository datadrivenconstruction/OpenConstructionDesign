// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The assistant end to end: a real worker thread, a scripted model, no network.
#![allow(dead_code)]

use crate::e2e_common::{assert_same, count, dump_except, hash_of, temp_file};
use ddc_ai::fake::{self, Script, ScriptedProvider};
use ddc_ai::{Limits, Mode, Part};
use ddc_app::ai::plan::{fingerprint, group_diff, PlanView};
use ddc_app::ai::session::AgentDraftView;
use ddc_app::ai::{SessionCfg, UiSession, ViewCtx};
use ddc_app::editor::Editor;
use ddc_gen::Generator;
use ddc_model::checks::CheckRule;
use ddc_model::{LayerSpec, Model, OpeningSpec, WallSpec};
use serde_json::{json, Value as J};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const WAIT: Duration = Duration::from_secs(120);
/// Caches and the assistant's own records: they are not the person's model.
pub const NOT_MODEL: &[&str] = &[
    "op",
    "ai_audit",
    "ai_policy",
    "ai_chat",
    "mesh_proto",
    "mesh_geo",
    "comp_thumb",
];

static SESSION: AtomicU64 = AtomicU64::new(0);

pub fn tool(name: &str, args: J) -> Script {
    Script::Tool {
        name: name.to_string(),
        args,
    }
}

/// One model answer per inner list: tools one by one, then a closing text.
pub fn script(tools: Vec<Script>) -> Vec<Vec<Script>> {
    let mut turns: Vec<Vec<Script>> = tools.into_iter().map(|t| vec![t]).collect();
    turns.push(vec![Script::Text("done".to_string())]);
    turns
}

pub fn view_of(ed: &Editor) -> ViewCtx {
    ViewCtx {
        view: "plan".to_string(),
        level: ed.level,
        units: ed.tool.units,
        selection: vec![],
        bounds: None,
        ..ViewCtx::default()
    }
}

pub fn cfg(provider: Arc<ScriptedProvider>) -> SessionCfg {
    SessionCfg {
        provider,
        profile: fake::profile(),
        limits: Limits::default(),
        session: format!("t{}", SESSION.fetch_add(1, Ordering::Relaxed)),
        allow_snapshots: Arc::new(AtomicBool::new(false)),
        extra_generators: vec![],
        extra_rules: vec![],
        keep_text: false,
        confirm_ttl: ddc_app::ai::session::CONFIRM_TTL,
        target: ddc_ai::catalog::Target::Main,
        auto_replan: false,
    }
}

pub fn live_hash(m: &Model) -> u64 {
    hash_of(&dump_except(m, NOT_MODEL))
}

pub fn start(ed: &mut Editor, c: SessionCfg) -> UiSession {
    let view = view_of(ed);
    let mut s = UiSession::start(ed, c, view).unwrap();
    assert!(s.wait_idle(ed, WAIT), "the session did not open");
    s
}

pub fn turn(s: &mut UiSession, ed: &mut Editor, text: &str, mode: Mode) {
    let view = view_of(ed);
    s.send_turn(ed, text.to_string(), view, mode).unwrap();
    assert!(s.wait_idle(ed, WAIT), "the turn did not finish");
}

pub fn run(ed: &mut Editor, tools: Vec<Script>) -> (UiSession, Arc<ScriptedProvider>) {
    let provider = ScriptedProvider::new(script(tools));
    let mut s = start(ed, cfg(provider.clone()));
    turn(&mut s, ed, "go", Mode::Propose);
    (s, provider)
}

pub fn plan_of(s: &UiSession) -> Arc<PlanView> {
    s.plan().expect("a plan").clone()
}

/// The results of every tool the model called, in order.
pub fn tool_results(p: &ScriptedProvider) -> Vec<(String, bool)> {
    let reqs = p.requests();
    let Some(last) = reqs.last() else {
        return vec![];
    };
    last.messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .filter_map(|x| match x {
            Part::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

/// The core of every scenario: the preview is what Apply did, in one undo step, undone exactly.
pub fn apply_and_check(s: &mut UiSession, ed: &mut Editor) -> i64 {
    // the person looks at an agent's items, and at a revised draft ("Got it"), before Apply
    if !plan_of(s).unreviewed.is_empty() || s.redraft().is_some() {
        let rev = s.plan_rev();
        s.review(Some(rev));
        assert!(s.wait_idle(ed, WAIT));
    }
    // a plan over the size cap is applied the way the card offers it: after "Allow large"
    let size_only = |p: &PlanView| {
        let mut blocks = p
            .issues
            .iter()
            .filter(|i| i.level == ddc_app::ai::validate::IssueLevel::Block)
            .peekable();
        blocks.peek().is_some()
            && blocks.all(|i| matches!(i.text_key, "ai.issue.too_large" | "ai.issue.delete_large"))
    };
    if !s.can_apply() && size_only(&plan_of(s)) {
        s.allow_large(true);
        assert!(s.wait_idle(ed, WAIT));
    }
    let plan = plan_of(s);
    assert!(s.can_apply(), "cannot apply: {:?}", plan.issues);
    let before = live_hash(&ed.model);
    let before_rows = dump_except(&ed.model, NOT_MODEL);
    let head = ed.model.change_head();
    let ops = count(&ed.model, "op");
    let snap = temp_file("ai_before.db");
    ed.model.snapshot_to(&snap).unwrap();

    let seq = s.apply(ed).unwrap();
    assert_eq!(seq, ed.model.op_top().unwrap());
    assert_eq!(
        count(&ed.model, "op"),
        ops + 1,
        "Apply must be one journal row"
    );

    let touched = ed.model.changes_since(head);
    let applied_diff = ed.model.diff_from(&snap, &touched, 200_000).unwrap();
    assert_eq!(
        fingerprint(&plan.diff),
        fingerprint(&group_diff(&applied_diff)),
        "preview differs from what was applied"
    );

    // the worker promotes its copy after Apply; reading the file earlier would race it
    assert!(s.wait_idle(ed, WAIT), "the worker did not take the Apply");
    assert_copy_is_live(s, ed);

    let applied = live_hash(&ed.model);
    assert_ne!(applied, before, "Apply changed nothing");
    ed.undo();
    assert_same(
        &dump_except(&ed.model, NOT_MODEL),
        &before_rows,
        "one undo must restore the exact state",
    );
    ed.redo();
    assert_eq!(
        live_hash(&ed.model),
        applied,
        "redo must bring the same state back"
    );
    seq
}

/// The card of `actor`'s draft once the worker has sent it.
pub fn agent_card(s: &mut UiSession, ed: &mut Editor, actor: &str) -> Arc<AgentDraftView> {
    let until = Instant::now() + WAIT;
    loop {
        s.pump_core(ed);
        if let Some(d) = s.agent_draft(actor) {
            return d.clone();
        }
        assert!(Instant::now() < until, "no draft card for {actor}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The person's Apply of an agent's draft as its card shows it: one journal row, exactly the
/// card's diff lands, the draft leaves the card, one undo restores the exact state, redo too.
pub fn apply_agent_and_check(s: &mut UiSession, ed: &mut Editor, actor: &str) -> i64 {
    assert!(s.wait_idle(ed, WAIT));
    let card = agent_card(s, ed, actor);
    assert!(!card.blocked(), "the card blocks Apply: {:?}", card.plan.issues);
    let before = live_hash(&ed.model);
    let head = ed.model.change_head();
    let ops = count(&ed.model, "op");
    let snap = temp_file("ai_agent_before.db");
    ed.model.snapshot_to(&snap).unwrap();

    let seq = s.apply_agent_draft(ed, actor, card.rev).unwrap();
    assert_eq!(seq, ed.model.op_top().unwrap());
    assert_eq!(count(&ed.model, "op"), ops + 1, "Apply must be one journal row");
    let touched = ed.model.changes_since(head);
    let applied_diff = ed.model.diff_from(&snap, &touched, 200_000).unwrap();
    assert_eq!(
        fingerprint(&card.plan.diff),
        fingerprint(&group_diff(&applied_diff)),
        "the card differs from what was applied"
    );
    assert!(s.wait_idle(ed, WAIT), "the worker did not take the Apply");
    assert!(s.agent_draft(actor).is_none(), "an applied draft leaves the card");

    let applied = live_hash(&ed.model);
    assert_ne!(applied, before, "Apply changed nothing");
    ed.undo();
    assert_eq!(live_hash(&ed.model), before, "one undo must restore the exact state");
    ed.redo();
    assert_eq!(live_hash(&ed.model), applied, "redo must bring the same state back");
    seq
}

pub fn assert_copy_is_live(s: &UiSession, ed: &Editor) {
    let work = Model::open_copy(&s.work_path().unwrap().to_string_lossy()).unwrap();
    assert_same(
        &dump_except(&ed.model, NOT_MODEL),
        &dump_except(&work, NOT_MODEL),
        "live differs from the scratch copy",
    );
}

pub fn add_level(ed: &mut Editor, name: &str, elevation: f64) -> i64 {
    let id = ed.model.add_level(name, elevation).unwrap();
    ed.reload().unwrap();
    id
}

/// Two walls meeting at a corner on the first level.
pub fn walls() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    add_wall(&mut ed, level, (0.0, 0.0), (10.0, 0.0));
    add_wall(&mut ed, level, (10.0, 0.0), (10.0, 6.0));
    ed
}

/// `walls()` with a 10 x 6 x 9 m box mass and the same box as a feature timeline beside it;
/// the editor, the plain mass id and the timeline mass id.
pub fn masses() -> (Editor, i64, i64) {
    let mut ed = walls();
    let level = ed.level;
    let mut boxed = |x: f64| {
        let ring = vec![(x, 10.0), (x + 10.0, 10.0), (x + 10.0, 16.0), (x, 16.0)];
        let form = ddc_model::Form::Extrude { ring, z0: 0.0, h: 9.0, ids: vec![] };
        let spec = ddc_model::MassSpec { level, name: "Box".into(), void: false, inplace: false, form };
        ed.model.add_mass(&spec).unwrap()
    };
    let plain = boxed(0.0);
    let tl = boxed(20.0);
    ed.model.mass_to_timeline(tl).unwrap();
    (ed, plain, tl)
}

pub fn add_wall(ed: &mut Editor, level: i64, a: (f64, f64), b: (f64, f64)) -> i64 {
    let spec = WallSpec {
        a,
        b,
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: ed.types[0].id,
        offset0: None,
    };
    let id = ed.model.add_walls(&[spec]).unwrap()[0];
    ed.reload().unwrap();
    id
}

pub fn add_door(ed: &mut Editor, wall: i64, along: f64) -> i64 {
    let spec = OpeningSpec {
        along,
        width: 0.9,
        sill: 0.0,
        height: 2.1,
        kind: "door".to_string(),
    };
    let id = ed.model.add_opening(wall, &spec).unwrap();
    ed.reload().unwrap();
    id
}

pub fn concrete(thickness: f64) -> Vec<LayerSpec> {
    vec![LayerSpec {
        material: "Concrete".to_string(),
        thickness,
    }]
}

pub fn with_generators(mut c: SessionCfg, g: Vec<&'static dyn Generator>) -> SessionCfg {
    c.extra_generators = g;
    c
}

/// A session on an option's own copy: the only place the stored "auto" opt-in takes effect.
pub fn as_draft(mut c: SessionCfg) -> SessionCfg {
    c.target = ddc_ai::catalog::Target::Draft;
    c
}

/// A draft session with the explicit opt-in stored, as an agent working an option sees it.
pub fn start_draft(ed: &mut Editor, provider: Arc<ScriptedProvider>) -> UiSession {
    // the yes of this session: a stored "auto" alone no longer counts
    ddc_app::ai::mcp_bridge::set_policy(ed, "test", ddc_ai::mcp::McpPolicy::AutoApply).unwrap();
    start(ed, as_draft(cfg(provider)))
}

pub fn with_rules(mut c: SessionCfg, r: Vec<&'static dyn CheckRule>) -> SessionCfg {
    c.extra_rules = r;
    c
}

pub fn parsed(text: &str) -> J {
    serde_json::from_str(text).unwrap_or_else(|e| json!({"unparsed": text, "error": e.to_string()}))
}

/// Library families of the wet fixtures and appliances the services join.
pub const WET_FAMILIES: [&str; 7] = [
    "WC",
    "Washbasin",
    "Bathtub",
    "Sink",
    "Shower Tray",
    "Washing Machine",
    "Dishwasher",
];
pub const APPLIANCE_FAMILIES: [&str; 3] = ["Washing Machine", "Dishwasher", "Cooker"];

/// Components placed from the given library families.
pub fn comps_of(m: &Model, families: &[&str]) -> Vec<i64> {
    let mut st = m
        .sql()
        .prepare(
            "SELECT c.id, f.name FROM comp c JOIN comp_type t ON t.id=c.ctype \
             JOIN comp_family f ON f.id=t.family ORDER BY c.id",
        )
        .unwrap();
    st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .map(|x| x.unwrap())
        .filter(|(_, n)| families.contains(&n.as_str()))
        .map(|(i, _)| i)
        .collect()
}

/// What the services of a built model hold.
#[derive(Debug, Default)]
pub struct SvcFacts {
    pub circuits: usize,
    pub wet: usize,
    pub appliances: usize,
    pub water_runs: usize,
    pub power_runs: usize,
}

/// Every network checks clean, every wet fixture (when `water`) is joined to its drain and cold water, every
/// appliance sits on a circuit, and every circuit is routed with a cable that carries its load.
pub fn assert_services(m: &Model, water: bool) -> SvcFacts {
    use ddc_model::{CircuitIssue, SystemKind};
    let mut f = SvcFacts::default();
    for c in m.mep_check_all().unwrap() {
        assert_eq!(c.errors(), 0, "system {}: {:?}", c.name, c.issues);
        match c.kind {
            SystemKind::Power => f.power_runs += c.runs.len(),
            SystemKind::ColdWater | SystemKind::HotWater | SystemKind::Sanitary => {
                f.water_runs += c.runs.len()
            }
            _ => {}
        }
    }
    let linked: std::collections::BTreeSet<(i64, String)> = m
        .mep_port_links(None)
        .unwrap()
        .into_iter()
        .filter(|l| l.system.is_some())
        .map(|l| (l.comp, l.connector))
        .collect();
    let ports = m.component_ports(None).unwrap();
    let wet = comps_of(m, &WET_FAMILIES);
    for id in wet.iter().filter(|_| water) {
        let joined: Vec<_> = ports
            .iter()
            .filter(|p| {
                p.comp == *id
                    && matches!(p.system, Some(SystemKind::Sanitary | SystemKind::ColdWater))
            })
            .collect();
        assert!(!joined.is_empty(), "wet fixture {id} has no water connector");
        for p in joined {
            assert!(
                linked.contains(&(*id, p.name.clone())),
                "wet fixture {id}: connector {} is not joined to a system",
                p.name
            );
        }
    }
    f.wet = wet.len();
    let on_circuit: std::collections::BTreeSet<i64> = {
        let mut st = m.sql().prepare("SELECT comp FROM elec_circuit_dev").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(|x| x.unwrap()).collect()
    };
    let appliances = comps_of(m, &APPLIANCE_FAMILIES);
    // with the electrical design switched off the appliances stand without circuits
    for id in appliances.iter().filter(|_| !m.circuits(None).unwrap().is_empty()) {
        assert!(on_circuit.contains(id), "appliance {id} is on no circuit");
    }
    f.appliances = appliances.len();
    for c in m.circuits(None).unwrap() {
        let rep = m.circuit_report(&c).unwrap();
        assert!(rep.cable > 0.0 && rep.breaker > 0.0, "circuit {} is not sized", rep.name);
        assert!(rep.devices > 0, "circuit {} has no device", rep.name);
        for i in &rep.issues {
            assert!(
                !matches!(i, CircuitIssue::Overload { .. } | CircuitIssue::NotRouted { .. }),
                "circuit {}: {i:?}",
                rep.name
            );
        }
        f.circuits += 1;
    }
    f
}

/// Ids of the pipes of the water systems.
pub fn water_run_ids(m: &Model) -> Vec<i64> {
    use ddc_model::SystemKind;
    let water: Vec<i64> = m
        .mep_systems()
        .unwrap()
        .into_iter()
        .filter(|s| matches!(s.kind, SystemKind::ColdWater | SystemKind::HotWater | SystemKind::Sanitary))
        .map(|s| s.id)
        .collect();
    let mut out = vec![];
    for s in water {
        out.extend(m.mep_runs(Some(s)).unwrap());
    }
    out.sort_unstable();
    out
}

/// Applies the session's plan, then holds the applied model to every oracle of the named case
/// file, each count oracle reported with its number.
pub fn meets_every_oracle(case_file: &str, s: &mut UiSession, ed: &mut Editor, answer: &str) {
    use ddc_app::ai::eval::case::{evals_dir, Case};
    use ddc_app::ai::eval::facts::Facts;
    use ddc_app::ai::eval::oracle::{evaluate, in_range, EvalCx};
    use ddc_app::ai::receipt::WriteReceipt;
    let case = Case::load(&evals_dir().join("cases").join(case_file)).unwrap();
    let plan = plan_of(s);
    let issues: Vec<_> = plan.issues.iter().chain(&plan.main_issues).cloned().collect();
    let seq = apply_and_check(s, ed);
    let receipts = vec![WriteReceipt::build(&plan.diff, &plan.qty, &issues, seq as u64, true, &case.id)];
    let facts = Facts::read(&ed.model).unwrap();
    let mut short = vec![];
    for c in &case.oracle.count {
        match facts.count(&c.spec) {
            Ok(n) if in_range(n as f64, c.min, c.max) => {}
            Ok(n) => short.push(format!("{}: {n} (min {:?}, max {:?})", c.spec, c.min, c.max)),
            Err(e) => short.push(format!("{}: {e}", c.spec)),
        }
    }
    assert!(short.is_empty(), "{}: count oracles: {short:#?}", case.id);
    let ifc = ddc_io::ifc::export_ifc(&ed.model).ok();
    let cx = EvalCx { ed, facts: &facts, before: None, answer, ifc: ifc.as_deref(), errors_before: 0, receipts: &receipts };
    let all = evaluate(&case, &cx);
    let failed: Vec<_> = all.iter().filter(|r| !r.pass).collect();
    assert!(!all.is_empty() && failed.is_empty(), "{}: {failed:#?}", case.id);
}
