//! The command registry: a stable schema, en/ru text for every command, coded errors only.
//! After an intended change, first raise the version of every command whose input or result
//! contract changed (descriptions are text, not contract), then refresh the snapshot:
//! `DDC_BLESS_SCHEMA=1 cargo test -p ddc-ai --test registry`. Blessing refuses a command whose
//! contract changed at the same version.

use ddc_ai::catalog::{self, Tier};
use ddc_ai::mcp::tools as mcp_tools;
use ddc_ai::registry::{self, Answer, Caller, CmdError, Effect, ErrorCode, Kind, Surface};
use ddc_ai::refusal::{self, RefusalCode, AI_CODES};
use ddc_ai::{CallCx, CancelToken, Mode, ToolHost, ToolOutcome};
use serde_json::{json, Value as J};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

const SNAPSHOT: &str = "tests/data/commands.v1.json";

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The wire contract of a schema: everything but its guidance text.
fn contract(v: &J) -> J {
    match v {
        J::Object(m) => J::Object(
            m.iter()
                .filter(|(k, x)| !(k.as_str() == "description" && x.is_string()))
                .map(|(k, x)| (k.clone(), contract(x)))
                .collect(),
        ),
        J::Array(a) => J::Array(a.iter().map(contract).collect()),
        x => x.clone(),
    }
}

/// Commands of `now` whose input or result contract differs from `saved` while the version
/// did not go up, or whose version went down.
fn unbumped(saved: &J, now: &J) -> Vec<String> {
    let old: BTreeMap<&str, &J> = saved["commands"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some((c["id"].as_str()?, c)))
        .collect();
    let mut bad = Vec::new();
    for c in now["commands"].as_array().into_iter().flatten() {
        let id = c["id"].as_str().unwrap_or_default();
        let Some(o) = old.get(id) else { continue };
        let (was, is) = (o["version"].as_u64().unwrap_or(0), c["version"].as_u64().unwrap_or(0));
        let changed: Vec<&str> =
            ["input", "result"].into_iter().filter(|f| contract(&o[*f]) != contract(&c[*f])).collect();
        if is < was || (!changed.is_empty() && is == was) {
            bad.push(format!("{id}: v{was} -> v{is}, changed: {changed:?}"));
        }
    }
    bad
}

#[test]
fn a_changed_contract_needs_a_higher_version() {
    let doc = |version: u64, max: u64, text: &str| {
        json!({ "commands": [{
            "id": "copy_elements",
            "version": version,
            "input": { "type": "object", "properties": {
                "offsets": { "description": text, "type": "array", "maxItems": max }
            } },
            "result": { "oneOf": [] }
        }] })
    };
    let saved = doc(1, 2, "Offsets in metres");
    assert_eq!(unbumped(&saved, &doc(1, 3, "Offsets in metres")).len(), 1, "widened at v1");
    assert_eq!(unbumped(&saved, &doc(1, 2, "Offsets, m")), Vec::<String>::new(), "text only");
    assert_eq!(unbumped(&saved, &doc(2, 3, "Offsets in metres")), Vec::<String>::new(), "bumped");
    assert_eq!(unbumped(&doc(2, 3, "x"), &doc(1, 3, "x")).len(), 1, "version went down");
    let mut renamed = doc(1, 2, "Offsets in metres");
    let p = renamed["commands"][0]["input"]["properties"].as_object_mut().unwrap();
    let v = p.remove("offsets").unwrap();
    p.insert("points".into(), v);
    assert_eq!(unbumped(&saved, &renamed).len(), 1, "renamed field at v1");
}

#[test]
fn the_schema_matches_its_snapshot() {
    let export = registry::export();
    let now = serde_json::to_string_pretty(&export).unwrap() + "\n";
    let path = manifest().join(SNAPSHOT);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let saved: J = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{SNAPSHOT}: {e}"));
        let bad = unbumped(&saved, &export);
        assert!(bad.is_empty(), "contract changed without a version bump (raise it in catalog.rs): {bad:#?}");
    }
    if std::env::var_os("DDC_BLESS_SCHEMA").is_some() {
        std::fs::write(&path, &now).unwrap();
    }
    let saved = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{SNAPSHOT} is missing ({e}): bless it with DDC_BLESS_SCHEMA=1"))
        .replace("\r\n", "\n");
    assert!(
        saved == now,
        "{SNAPSHOT} differs from the registry; rerun with DDC_BLESS_SCHEMA=1, review the diff \
         and bump the version of every command whose input or result changed"
    );
}

#[test]
fn every_tool_is_one_registered_command() {
    let ids: Vec<&str> = registry::commands().iter().map(|c| c.id()).collect();
    let unique: HashSet<&str> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "ids are unique");
    assert_eq!(ids.len(), catalog::catalog().len() + mcp_tools::only_here().len());
    for c in registry::commands() {
        assert!(c.version >= 1, "{}", c.id());
        assert!(
            c.id().bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "{}: ids are snake case",
            c.id()
        );
        let chat = catalog::find(c.id()).is_some();
        assert_eq!(c.surface == Surface::All, chat, "{}", c.id());
        // kind, version, effect and answer come from the one definition and agree
        let s = c.spec;
        assert_eq!((c.kind, c.version), (s.kind, s.version), "{}", c.id());
        assert!(!(s.read_only() && s.destructive()), "{}", c.id());
        if s.destructive() {
            assert!(matches!(c.kind, Kind::Edit | Kind::Session), "{}", c.id());
        }
        if matches!(c.kind, Kind::Query | Kind::View) {
            assert_eq!(s.effect, Effect::Nothing, "{}: a query or view changes nothing", c.id());
        }
        if c.kind == Kind::Edit {
            assert!(!s.read_only(), "{}: an edit is never read-only", c.id());
        }
        if s.answer == Answer::PlanItem {
            assert_eq!(c.kind, Kind::Edit, "{}: only an edit adds a plan item", c.id());
        }
        if s.access == catalog::Access::Read {
            assert!(s.read_only(), "{}: the reading tier changes nothing", c.id());
        }
        assert!(c.input_schema()["type"] == "object", "{}", c.id());
        assert!(c.result_schema()["oneOf"].as_array().is_some_and(|a| a.len() == 2));
    }
    for id in ["query_elements", "get_element", "schedule", "list_params"] {
        assert_eq!(registry::find(id).map(|c| c.kind), Some(Kind::Query), "{id}");
    }
    for id in ["update_elements", "create_elements", "delete_elements", "replace_type"] {
        assert_eq!(registry::find(id).map(|c| c.kind), Some(Kind::Edit), "{id}");
    }
    let commit = registry::find(mcp_tools::CHANGESET_COMMIT).unwrap();
    assert!(commit.spec.destructive(), "a commit may carry a deletion");
    let item = |id| registry::find(id).map(|c| c.spec.answer);
    assert_eq!(item("create_grids"), Some(Answer::PlanItem));
    assert_eq!(item("apply_fix"), Some(Answer::Data), "one call, several items");
    let export = registry::export();
    let rows = export["commands"].as_array().unwrap();
    assert_eq!(rows.len(), registry::commands().len());
    for (c, row) in registry::commands().iter().zip(rows) {
        assert_eq!(row["id"], c.id());
        assert_eq!(row["destructive"], c.spec.destructive(), "{}", c.id());
        assert_eq!(row["read_only"], c.spec.read_only(), "{}", c.id());
    }
    for t in mcp_tools::list_json().as_array().unwrap() {
        let c = registry::find(t["name"].as_str().unwrap()).unwrap();
        let a = &t["annotations"];
        assert_eq!(a["destructiveHint"], c.spec.destructive(), "{}", c.id());
        assert_eq!(a["readOnlyHint"], c.spec.read_only(), "{}", c.id());
    }
}

fn catalog_of(lang: &str) -> BTreeMap<String, String> {
    let dir = manifest().join("../ui/data/i18n").join(lang);
    let mut out = BTreeMap::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.push(manifest().join(format!("../ui/data/i18n/commands.{lang}.json")));
    for f in files {
        let text = std::fs::read_to_string(Path::new(&f)).unwrap();
        let v: BTreeMap<String, J> = serde_json::from_str(&text).unwrap();
        for (k, v) in v {
            if let Some(s) = v.as_str() {
                out.insert(k, s.to_string());
            }
        }
    }
    out
}

#[test]
fn every_command_has_en_and_ru_text() {
    let en = catalog_of("en");
    let ru = catalog_of("ru");
    let cyrillic = |s: &str| s.chars().any(|c| ('\u{0400}'..='\u{04ff}').contains(&c));
    for c in registry::commands() {
        let key = c.text_key();
        let e = en.get(&key).unwrap_or_else(|| panic!("no en text {key}"));
        let r = ru.get(&key).unwrap_or_else(|| panic!("no ru text {key}"));
        assert!(!e.trim().is_empty() && !cyrillic(e), "{key}: en");
        assert!(cyrillic(r), "{key}: ru is translated");
    }
}

#[test]
fn the_agent_list_carries_versions() {
    for t in mcp_tools::list_json().as_array().unwrap() {
        let id = t["name"].as_str().unwrap();
        let c = registry::find(id).unwrap();
        assert_eq!(t["_meta"]["ddc/version"], json!(c.version), "{id}");
        assert_eq!(t["_meta"]["ddc/kind"], json!(c.kind.as_str()), "{id}");
    }
}

/// Answers with whatever was queued; records what reached it.
#[derive(Default)]
struct Host {
    calls: Vec<String>,
    answer: Option<ToolOutcome>,
}

impl ToolHost for Host {
    fn call(&mut self, name: &str, _args: &J, _cx: &CallCx<'_>) -> ToolOutcome {
        self.calls.push(name.to_string());
        self.answer.clone().unwrap_or_else(|| ToolOutcome::ok("{\"untrusted\":\"data\"}"))
    }
}

fn run(host: &mut Host, id: &str, args: J, mode: Mode) -> ToolOutcome {
    let cancel = CancelToken::new();
    let cx = CallCx {
        mode,
        cancel: &cancel,
        actor: "test",
        lang: "en",
    };
    registry::call(host, id, &args, &cx, true)
}

fn code_of(out: &ToolOutcome) -> ErrorCode {
    assert!(out.is_error, "{}", out.text);
    let v: J = serde_json::from_str(&out.text).expect("an error is JSON");
    assert!(out.text.starts_with("{\"untrusted\":\"data\""), "{}", out.text);
    ErrorCode::parse(v["code"].as_str().expect("a code")).expect("a known code")
}

#[test]
fn gate_failures_are_coded_and_never_reach_the_host() {
    let mut h = Host::default();
    let out = run(&mut h, "execute_sql", json!({}), Mode::Propose);
    assert_eq!(code_of(&out), ErrorCode::UnknownCommand);
    // an agent-only command is not reachable through the chat path
    let out = run(&mut h, mcp_tools::OPS_PROPOSE, json!({"envelope": {}}), Mode::Propose);
    assert_eq!(code_of(&out), ErrorCode::UnknownCommand);
    let edit = json!({"title": "t", "target": {"refs": ["wall:1"]}});
    let out = run(&mut h, "delete_elements", edit.clone(), Mode::ReadOnly);
    assert_eq!(code_of(&out), ErrorCode::NotAllowed);
    let out = run(&mut h, "delete_elements", json!({"title": 5}), Mode::Propose);
    assert_eq!(code_of(&out), ErrorCode::InvalidArgs);
    // a caller without pictures is refused the picture tools
    let no_images = registry::gate_as(catalog::VIEW_SNAPSHOT, &json!({}), Caller::Chat,
        Tier::T1Propose, false);
    assert_eq!(no_images.unwrap_err().code, ErrorCode::NotAllowed);
    let e = registry::error_of(&out).unwrap();
    assert!(e.path.as_deref().is_some_and(|p| p.starts_with("args")), "{e:?}");
    assert!(h.calls.is_empty(), "{:?}", h.calls);
    let out = run(&mut h, "delete_elements", edit, Mode::Propose);
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(h.calls, vec!["delete_elements"]);
}

/// Every command, one by one: bad arguments, a forbidden mode and a failing host all come back
/// coded, and only a call that passed the gate reaches the host.
#[test]
fn every_command_has_a_coded_error_path() {
    let bogus = json!({"no_such_argument": 1});
    for c in registry::commands() {
        let id = c.id();
        let e = c.validate(&bogus).expect_err(id);
        assert_eq!(e.code, ErrorCode::InvalidArgs, "{id}: {e:?}");
        assert!(e.path.is_some(), "{id}: the path is named: {e:?}");
        let mut h = Host::default();
        let out = run(&mut h, id, bogus.clone(), Mode::Propose);
        let want = match c.surface {
            Surface::All => ErrorCode::InvalidArgs,
            Surface::AgentOnly => ErrorCode::UnknownCommand,
        };
        assert_eq!(code_of(&out), want, "{id}");
        if c.surface == Surface::All && c.spec.tier() > catalog::Tier::T0Read {
            let out = run(&mut h, id, bogus.clone(), Mode::ReadOnly);
            assert_eq!(code_of(&out), ErrorCode::NotAllowed, "{id}");
        }
        // agents and the command line reach every command; their bad arguments are coded too
        for caller in [Caller::Agent, Caller::Cli] {
            let e = registry::gate_as(id, &bogus, caller, Tier::T1Propose, true).expect_err(id);
            assert_eq!(e.code, ErrorCode::InvalidArgs, "{id}: {caller:?}");
        }
        assert!(h.calls.is_empty(), "{id}: a refused call never reaches the host");
    }
}

#[test]
fn every_caller_reaches_exactly_its_surface() {
    let args = |id: &str| match id {
        mcp_tools::EXPORT_IFC => json!({"file": "model.ifc"}),
        _ => json!({}),
    };
    let gate = |id: &str, caller| registry::gate_as(id, &args(id), caller, Tier::T1Propose, true);
    let chat = gate(mcp_tools::EXPORT_IFC, Caller::Chat).unwrap_err();
    assert_eq!(chat.code, ErrorCode::UnknownCommand, "the chat exports through its own tools");
    for caller in [Caller::Agent, Caller::Cli] {
        let cmd = gate(mcp_tools::EXPORT_IFC, caller)
            .unwrap_or_else(|e| panic!("{caller:?}: {e:?}"));
        assert_eq!(cmd.id(), mcp_tools::EXPORT_IFC);
        assert!(gate(mcp_tools::PROJECT_INFO, caller).is_ok(), "{caller:?}");
    }
    let edit = json!({"title": "t", "target": {"refs": ["wall:1"]}});
    let e = registry::gate_as("delete_elements", &edit, Caller::Cli, Tier::T0Read, false);
    assert_eq!(e.unwrap_err().code, ErrorCode::NotAllowed, "the ceiling binds every caller");
    let ok = registry::gate_as("delete_elements", &edit, Caller::Cli, Tier::T1Propose, false);
    assert!(ok.is_ok(), "{ok:?}");
}

#[test]
fn a_cancelled_call_is_coded() {
    let mut h = Host::default();
    let cancel = CancelToken::new();
    cancel.cancel();
    let cx = CallCx {
        mode: Mode::Propose,
        cancel: &cancel,
        actor: "test",
        lang: "en",
    };
    let out = registry::call(&mut h, "list_levels", &json!({}), &cx, false);
    assert_eq!(code_of(&out), ErrorCode::Cancelled);
    assert!(h.calls.is_empty());
}

#[test]
fn host_errors_get_a_code_and_keep_their_fields() {
    let mut h = Host {
        answer: Some(ToolOutcome::error("no element door:7 on level 2")),
        ..Host::default()
    };
    let out = run(&mut h, "get_element", json!({"ref": "door:7"}), Mode::Propose);
    assert_eq!(code_of(&out), ErrorCode::NotFound);
    assert_eq!(registry::error_of(&out).unwrap().refs, vec!["door:7"]);

    h.answer = Some(ToolOutcome::error(
        "{\"untrusted\":\"data\",\"status\":\"stale\",\"draft\":{\"n\":3}}",
    ));
    let out = run(&mut h, "list_levels", json!({}), Mode::Propose);
    assert_eq!(code_of(&out), ErrorCode::Conflict);
    let v: J = serde_json::from_str(&out.text).unwrap();
    assert_eq!(v["draft"]["n"], 3, "the host's fields stay");

    let coded = CmdError::new(ErrorCode::Limit, "too many").to_text();
    h.answer = Some(ToolOutcome::error(coded.clone()));
    let out = run(&mut h, "list_levels", json!({}), Mode::Propose);
    assert_eq!(out.text, coded, "a coded error passes unchanged");
}

#[test]
fn handler_wording_maps_to_codes() {
    let cases = [
        ("no_effect: the call changes nothing", ErrorCode::NoEffect),
        ("nothing is selected", ErrorCode::NoEffect),
        ("not_allowed", ErrorCode::NotAllowed),
        ("cancelled", ErrorCode::Cancelled),
        ("unknown tool", ErrorCode::UnknownCommand),
        ("args.items[0].cat: expected string", ErrorCode::InvalidArgs),
        ("set must be an object", ErrorCode::InvalidArgs),
        ("no level 12: see list_levels", ErrorCode::NotFound),
        ("the draft is stale", ErrorCode::Conflict),
        ("too many elements", ErrorCode::Limit),
        ("disk full", ErrorCode::Failed),
    ];
    for (text, want) in cases {
        assert_eq!(CmdError::from_text(text).code, want, "{text}");
    }
    // a handler that names its code is read back exactly, whatever its words say
    for code in registry::ERROR_CODES {
        let e = CmdError::from_text(&CmdError::wire(code, "too many stale things gone"));
        assert_eq!((e.code, e.message.as_str()), (code, "too many stale things gone"));
    }
    let e = CmdError::from_text("no_effect: left out wall:3 x; door:9 y; wall:3 again");
    assert_eq!(e.message, "left out wall:3 x; door:9 y; wall:3 again");
    assert_eq!(e.refs, vec!["wall:3", "door:9"]);
    let e = CmdError::from_text("args.target.refs[1]: expected string");
    assert_eq!(e.path.as_deref(), Some("args.target.refs[1]"));
}

#[test]
fn the_error_schema_lists_every_code() {
    let schema = registry::error_schema();
    let listed: Vec<&str> = schema["properties"]["code"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let names: Vec<&str> = registry::ERROR_CODES.iter().map(|c| c.as_str()).collect();
    assert_eq!(listed, names);
    let unique: HashSet<&str> = names.iter().copied().collect();
    assert_eq!(unique.len(), names.len(), "codes are distinct words");
    for name in ["busy", "no_effect", "not_found", "invalid_args"] {
        assert!(names.contains(&name), "{name}");
    }
}

#[test]
fn every_refusal_code_round_trips() {
    for c in AI_CODES {
        let out = CmdError::refused(refusal::of(c)).outcome();
        let back = registry::error_of(&out).expect("a failure");
        assert_eq!(back.refusal().code, c, "{}", out.text);
        assert_eq!(back.code, ErrorCode::of_refusal(c), "{c:?} keeps its registry code");
        let again = registry::ensure_coded_in(out.clone(), "de");
        assert_eq!(registry::error_of(&again).unwrap().refusal().code, c, "{}", again.text);
    }
    for e in registry::ERROR_CODES {
        let r = e.refusal_code();
        assert!(AI_CODES.contains(&r), "{e:?} -> {r:?}");
        assert_eq!(ErrorCode::of_refusal(r), e, "{e:?} maps back through {r:?}");
    }
    // stale is named by the reply, never guessed from its words
    let worded = CmdError::new(ErrorCode::Conflict, "the plan is stale");
    assert_eq!(worded.refusal().code, RefusalCode::PlanConflict);
    let named = registry::ensure_coded(ToolOutcome::error("{\"untrusted\":\"data\",\"status\":\"stale\"}"));
    assert_eq!(registry::error_of(&named).unwrap().refusal().code, RefusalCode::PlanStale);
    assert_eq!(CmdError::new(ErrorCode::NoEffect, "x").refusal().code, RefusalCode::NoEffect);
}

#[test]
fn a_coded_reply_without_words_still_has_a_message() {
    let out = ToolOutcome::error(r#"{"untrusted":"data","status":"busy","code":"busy"}"#);
    let e = registry::error_of(&out).unwrap();
    assert_eq!(e.code, ErrorCode::Busy);
    assert_eq!(e.message, "busy", "the status names it");
    let out = ToolOutcome::error(r#"{"untrusted":"data","code":"limit"}"#);
    assert_eq!(registry::error_of(&out).unwrap().message, "limit", "the code names it");
}

fn refused_code(out: &ToolOutcome) -> String {
    let v: J = serde_json::from_str(&out.text).unwrap_or_else(|_| panic!("not JSON: {}", out.text));
    let r = &v["refused"];
    for k in ["message", "try"] {
        assert!(r[k].as_str().is_some_and(|s| !s.is_empty()), "{k} in {}", out.text);
    }
    r["code"].as_str().unwrap_or_else(|| panic!("no refused.code: {}", out.text)).to_string()
}

/// Codes whose producer lands in a later card; each card removes its own from this list.
const PENDING: [RefusalCode; 5] = [
    RefusalCode::DraftInvalid, // 124-127
    RefusalCode::TextTooLarge,
    RefusalCode::UnknownFamily,
    RefusalCode::RepairStopped,
    RefusalCode::NameTaken,
];

/// Codes a real editor handler answers: reached in `ddc-app` (`e2e_ai_invariants`
/// `snapshot_refusals_come_from_the_handler`), where the handler exists.
const IN_APP: [RefusalCode; 2] = [RefusalCode::SnapshotsOff, RefusalCode::SnapshotUnavailable];

#[test]
fn every_ai_code_reachable() {
    let mut got: Vec<String> = Vec::new();
    let host = |answer: ToolOutcome, id: &str, args: J, mode: Mode| {
        let mut h = Host { answer: Some(answer), ..Host::default() };
        run(&mut h, id, args, mode)
    };
    let ok = ToolOutcome::ok("{\"untrusted\":\"data\"}");
    let edit = json!({"title": "t", "target": {"refs": ["wall:1"]}});
    // the gate's own refusals
    got.push(refused_code(&host(ok.clone(), "delete_elements", json!({"title": 5}), Mode::Propose)));
    got.push(refused_code(&host(ok.clone(), "delete_elements", edit.clone(), Mode::ReadOnly)));
    let table = [catalog::Retired { name: "old_list_levels", replacement: "list_levels", since: 2 }];
    let e = registry::gate_in(&table, "old_list_levels", &json!({}), Caller::Chat, Tier::T1Propose, true)
        .unwrap_err();
    got.push(refused_code(&e.outcome()));
    let e = registry::gate_in(&[], "no_such_tool", &json!({}), Caller::Chat, Tier::T1Propose, true).unwrap_err();
    got.push(refused_code(&e.outcome()));
    let cancel = CancelToken::new();
    cancel.cancel();
    let cx = CallCx { mode: Mode::Propose, cancel: &cancel, actor: "test", lang: "en" };
    got.push(refused_code(&registry::call(&mut Host::default(), "list_levels", &json!({}), &cx, true)));
    // what hosts answer: older wording and status replies
    let answers = [
        ToolOutcome::error("no element door:7 on level 2"),
        ToolOutcome::error("{\"untrusted\":\"data\",\"status\":\"busy\",\"code\":\"busy\"}"),
        ToolOutcome::error("{\"untrusted\":\"data\",\"status\":\"stale\"}"),
        ToolOutcome::error("the card changed since: conflict with the person's review"),
        ToolOutcome::error("too many elements"),
        ToolOutcome::error("disk full"),
        ToolOutcome::error("{\"untrusted\":\"data\",\"code\":\"no_effect\",\"error\":\"already there\"}"),
        ToolOutcome::error(refusal::wire(&refusal::of(RefusalCode::NeedsConfirmation))),
        ToolOutcome::error(refusal::wire(
            &refusal::of(RefusalCode::MassDeleteBlocked).arg("n", "900").arg("of", "1000"),
        )),
        ToolOutcome::error(refusal::wire(
            &refusal::of(RefusalCode::SinceExpired).fix(refusal::call_tool("run_checks", vec![])),
        )),
    ];
    for a in answers {
        got.push(refused_code(&host(a, "list_levels", json!({}), Mode::Propose)));
    }
    let want: Vec<&str> = AI_CODES
        .iter()
        .filter(|c| !PENDING.contains(c) && !IN_APP.contains(c))
        .map(|c| c.as_str())
        .collect();
    for w in &want {
        assert!(got.iter().any(|g| g == w), "{w} is never answered; got {got:?}");
    }
    for g in &got {
        assert!(RefusalCode::parse(g).is_some(), "{g} is not a refusal code");
    }
}

#[test]
fn snapshots_off_names_the_settings_fix() {
    let r = refusal::of(RefusalCode::SnapshotsOff).fix(refusal::open_settings());
    let mut h = Host { answer: Some(ToolOutcome::error(refusal::wire(&r))), ..Host::default() };
    let out = run(&mut h, "list_levels", json!({}), Mode::Propose);
    let v: J = serde_json::from_str(&out.text).unwrap();
    assert_eq!(v["code"], "not_allowed", "the older code stays for older readers");
    assert_eq!(v["refused"]["code"], "snapshots_off");
    assert_eq!(v["refused"]["fix"]["command"], "ai.settings");
    assert_eq!(registry::error_of(&out).unwrap().refusal(), r, "read back exactly");
}

#[test]
fn retired_not_in_catalog() {
    for r in catalog::RETIRED.iter() {
        assert!(mcp_tools::find(r.name).is_none(), "{} is retired but still a tool", r.name);
        assert!(mcp_tools::find(r.replacement).is_some(), "{}: replacement {} is no tool", r.name, r.replacement);
        assert!(r.since >= 1, "{}", r.name);
    }
    // a name back in the catalog is live again, whatever the table says
    let back = [catalog::Retired { name: "list_levels", replacement: "get_element", since: 2 }];
    assert!(catalog::retired_refusal(&back, "list_levels").is_none());
    let gone = [catalog::Retired { name: "old_list_levels", replacement: "list_levels", since: 2 }];
    let r = catalog::retired_refusal(&gone, "old_list_levels").unwrap();
    assert_eq!((r.code, r.arg_of("replacement")), (RefusalCode::ToolRetired, Some("list_levels")));
    assert_eq!(r.fixes[0].command, "list_levels");
}

#[test]
fn retired_never_listed() {
    let listed: HashSet<String> = mcp_tools::list_json()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for mode in [Mode::ReadOnly, Mode::Propose] {
        for s in catalog::schemas(mode, true) {
            assert!(catalog::RETIRED.iter().all(|r| r.name != s.name), "{} offered to the chat", s.name);
        }
    }
    for r in catalog::RETIRED.iter() {
        assert!(!listed.contains(r.name), "{} offered to agents", r.name);
        assert!(registry::find(r.name).is_none(), "{} still in the registry", r.name);
    }
    // a retired name in a table: refused with its replacement by the gate, offered nowhere
    let table = [catalog::Retired { name: "old_list_levels", replacement: "list_levels", since: 2 }];
    assert!(!listed.contains(table[0].name));
    let e = registry::gate_in(&table, table[0].name, &json!({}), Caller::Agent, Tier::T1Propose, true)
        .unwrap_err();
    let v: J = serde_json::from_str(&e.outcome().text).unwrap();
    assert_eq!(v["refused"]["code"], "tool_retired");
    assert_eq!(v["refused"]["fix"]["command"], "list_levels");
    assert_eq!(v["code"], "unknown_command", "older readers see an unknown command");
    // the same name with no table entry is simply unknown
    let e = registry::gate_as(table[0].name, &json!({}), Caller::Agent, Tier::T1Propose, true).unwrap_err();
    assert_ne!(e.refusal().code, RefusalCode::ToolRetired);
}

#[test]
fn refusal_texts_follow_the_session_language() {
    let call = |lang: &str| {
        let cancel = CancelToken::new();
        let cx = CallCx { mode: Mode::ReadOnly, cancel: &cancel, actor: "test", lang };
        let edit = json!({"title": "t", "target": {"refs": ["wall:1"]}});
        registry::call(&mut Host::default(), "delete_elements", &edit, &cx, true)
    };
    let (en, ru) = (call("en"), call("ru"));
    let (ve, vr): (J, J) = (serde_json::from_str(&en.text).unwrap(), serde_json::from_str(&ru.text).unwrap());
    assert!(ru.text.starts_with("{\"untrusted\":\"data\""), "{}", ru.text);
    assert_eq!(ve["refused"]["code"], vr["refused"]["code"], "the code is never translated");
    assert_eq!(ve["code"], vr["code"]);
    let cyr = |v: &J| v["refused"]["try"].as_str().unwrap().chars().any(|c| ('\u{0430}'..='\u{044f}').contains(&c));
    assert!(!cyr(&ve) && cyr(&vr), "{} / {}", en.text, ru.text);
}
