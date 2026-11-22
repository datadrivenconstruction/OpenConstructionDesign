//! The chat steers a generated project through its rules: a scripted model reads, sets and
//! locks rules, asks why and why not, compares variants and regenerates. Every change is a
//! draft applied in one undo step; a value out of range or on a locked rule changes nothing.

use crate::e2e_ai_common;

use ddc_ai::fake::Script;
use ddc_app::ai::project_plan;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::{json, Value as J};

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

fn stored_input(ed: &Editor) -> J {
    let s = project_plan::stored(&ed.model).unwrap();
    s.last().expect("a stored project").1["input"].clone()
}

fn calls() -> Vec<Script> {
    vec![
        tool("generate_project", json!({"spec": {"typology": "office", "floors": 2}})),
        // 0..=1: refused, nothing changes
        tool("set_rule", json!({"rules": {"massing.floor_height": 99}})),
        tool("set_rule", json!({"rules": {"massing.floor_height": 3.3}})),
        tool("lock", json!({"targets": [{"rule": "grid.span"}]})),
        tool("set_rule", json!({"rules": {"grid.span": 6.0}})),
        tool("list_rules", json!({"changed": true})),
        tool("get_rule", json!({"id": "massing.floor_height"})),
        tool("explain", json!({"ref": "massing"})),
        tool("compare_variants", json!({"n": 2, "vary": ["massing.form", "facade.system"]})),
        tool(
            "conflicts",
            json!({"changes": {"rules": {"goal.efficiency": {"value": 0.98, "hard": true}, "room.door": 1.0}}}),
        ),
    ]
}

#[test]
fn rules_are_set_locked_explained_and_applied_in_one_step() {
    let mut ed = editor();
    let (mut s, provider) = run(&mut ed, calls());
    let r = tool_results(&provider);
    assert_eq!(r.len(), 10, "{r:?}");
    let errors: Vec<usize> = r.iter().enumerate().filter(|x| x.1 .1).map(|x| x.0).collect();
    assert_eq!(errors, vec![1, 4], "out of range and locked are refused, the rest pass: {r:?}");
    assert!(r[1].0.contains("massing.floor_height"), "{}", r[1].0);
    assert!(r[4].0.contains("locked"), "{}", r[4].0);
    // every answer carries its reason
    for (i, x) in r.iter().enumerate().skip(1).filter(|x| !x.1 .1) {
        assert!(parsed(&x.0)["why"].is_string(), "{i}: {}", x.0);
    }
    let set = parsed(&r[2].0);
    assert_eq!(set["revision"]["item"], 1, "the rule change revises the drafted item: {set}");
    assert_eq!(plan_of(&s).items.len(), 1, "one item, revised in place");
    let listed = parsed(&r[5].0);
    let ids: Vec<&str> = listed["rules"].as_array().unwrap().iter().filter_map(|x| x["id"].as_str()).collect();
    assert!(ids.contains(&"massing.floor_height"), "{listed}");
    let locked = listed["rules"].as_array().unwrap().iter().any(|x| x["id"] == "grid.span" && x["locked"] == true);
    assert!(locked || !ids.contains(&"grid.span"), "{listed}");
    assert!(parsed(&r[7].0)["reasons"].as_array().is_some_and(|a| !a.is_empty()), "{}", r[7].0);
    assert!(parsed(&r[8].0)["compare"]["variants"].as_array().is_some_and(|a| !a.is_empty()), "{}", r[8].0);
    let why_not = parsed(&r[9].0);
    assert_eq!(why_not["feasible"], false, "{why_not}");
    assert!(why_not["relax"].as_array().is_some_and(|a| a.iter().all(|f| f["set_rule"].is_object())));
    apply_and_check(&mut s, &mut ed);
    let input = stored_input(&ed);
    assert_eq!(input["rules"]["massing.floor_height"], json!(3.3), "{input}");
    assert_eq!(input["locks"][0]["rule"], "grid.span", "{input}");
    drop(s);

    // the applied project: release the lock, regenerate with a change, apply again
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool("unlock", json!({"all": true})),
            tool("regenerate", json!({"changes": {"floors": 3}})),
        ],
    );
    let r = tool_results(&provider);
    assert!(r.iter().all(|x| !x.1), "{r:?}");
    apply_and_check(&mut s, &mut ed);
    let input = stored_input(&ed);
    assert!(input.get("locks").is_none(), "{input}");
    assert_eq!(input["floors"], 3, "{input}");
}

/// The person's rule travels in the brief, on the terrain the chat made: the rule is locked,
/// a later change of it is refused, and the stored brief keeps rule, lock and the word "model".
#[test]
fn a_rule_in_the_brief_is_locked_on_the_chat_made_terrain() {
    let mut ed = editor();
    let ground = json!({"title": "Ground", "boundary": [[-60, -60], [160, -60], [160, 140], [-60, 140]],
                        "z": 50.0, "slope": 10.0, "downhill": 0.0});
    let (mut s, provider) = run(&mut ed, vec![tool("create_terrain", ground)]);
    assert!(tool_results(&provider).iter().all(|x| !x.1));
    apply_and_check(&mut s, &mut ed);
    drop(s);
    assert_eq!(ed.model.toposolids().unwrap().len(), 1);
    let brief = json!({"typology": "office", "floors": 2, "origin": [10.0, 5.0],
                       "rules": {"massing.floor_height": 3.3},
                       "site": {"terrain": "model", "pad_margin": 0}});
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool("generate_project", json!({"spec": brief})),
            tool("lock", json!({"targets": [{"rule": "massing.floor_height"}]})),
            tool("set_rule", json!({"rules": {"massing.floor_height": 3.6}})),
        ],
    );
    let r = tool_results(&provider);
    assert!(!r[0].1 && !r[1].1, "{r:?}");
    assert!(r[2].1 && r[2].0.contains("locked"), "a locked rule is not changed: {}", r[2].0);
    apply_and_check(&mut s, &mut ed);
    let input = stored_input(&ed);
    assert_eq!(input["rules"]["massing.floor_height"], json!(3.3), "{input}");
    assert_eq!(input["locks"][0]["rule"], "massing.floor_height", "{input}");
    assert_eq!(input["site"]["terrain"], "model", "{input}");
}

/// A new project is refined by default within `layout.refine_rounds`, never over the person's
/// own choice, and applied in one undo step; the rule at 0 or the setting off drafts it as given.
#[test]
fn a_new_project_is_refined_by_default_within_its_rounds() {
    let generate = |ed: &mut Editor, spec: J| {
        let (mut s, provider) = run(ed, vec![tool("generate_project", json!({"spec": spec}))]);
        let r = tool_results(&provider);
        assert!(!r[0].1, "{}", r[0].0);
        apply_and_check(&mut s, ed);
        (parsed(&r[0].0), stored_input(ed))
    };
    let mut ed = editor();
    let (reply, input) = generate(&mut ed, json!({"typology": "office", "floors": 2, "design": {"facade": "ribbon"}}));
    let rounds = reply["refined"]["rounds"].as_array().map_or(0, Vec::len);
    assert!((1..=3).contains(&rounds), "{reply}");
    assert_eq!(input["design"]["facade"], "ribbon", "the person's facade stays: {input}");
    assert_eq!(input["design"]["canopy"], true, "an open field is filled: {input}");

    let mut ed = editor();
    let spec = json!({"typology": "office", "floors": 2, "rules": {"layout.refine_rounds": 0}});
    let (reply, input) = generate(&mut ed, spec);
    assert!(reply.get("refined").is_none(), "{reply}");
    assert!(input["design"]["canopy"].is_null(), "{input}");

    let mut ed = editor();
    ed.model.set_ai_policy(ddc_app::ai::project_tools::POLICY_REFINE, "0").unwrap();
    let (reply, input) = generate(&mut ed, json!({"typology": "office", "floors": 2}));
    assert!(reply.get("refined").is_none(), "{reply}");
    assert!(input.get("design").is_none_or(|d| d["canopy"].is_null()), "{input}");
}

#[test]
fn a_change_a_storey_lock_forbids_is_reported_not_applied() {
    let mut ed = editor();
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool("generate_project", json!({"spec": {"typology": "office", "floors": 2}})),
            tool("lock", json!({"targets": [{"storey": 0, "keep": "geometry"}]})),
            tool("regenerate", json!({"changes": {"rules": {"grid.span": 6.0}, "floors": 3}})),
        ],
    );
    let r = tool_results(&provider);
    assert!(r.iter().all(|x| !x.1), "{r:?}");
    let g = parsed(&r[2].0);
    let refused = g["locks"]["refused"].as_array().cloned().unwrap_or_default();
    let applied = g["locks"]["applied"].as_array().cloned().unwrap_or_default();
    assert!(!applied.is_empty() || !refused.is_empty(), "{g}");
    for x in &refused {
        assert!(x["because"].to_string().contains("storey 0"), "{x}");
    }
    apply_and_check(&mut s, &mut ed);
    assert!(stored_input(&ed)["locks"].is_array());
}

/// A large building's reply still carries its reasons and its brief check: a long summary
/// sheds warnings, score and metrics first and names what it shed.
#[test]
fn a_large_project_reply_keeps_why_and_the_brief_check() {
    let briefs = [
        json!({"typology": "hotel", "floors": 5}),
        json!({"typology": "mixed_use", "floors": 7, "design": {"massing": "U"}}),
        json!({"typology": "office", "floors": 14, "services": true}),
        json!({"typology": "apartment_block", "floors": 6}),
    ];
    for b in briefs {
        let mut ed = editor();
        let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": b}))]);
        let r = tool_results(&provider);
        assert!(!r[0].1, "{b}: {}", r[0].0);
        let reply = parsed(&r[0].0);
        let p = &reply["project"];
        assert!(p["why"].as_array().is_some_and(|w| !w.is_empty()), "{b}: {reply}");
        assert!(p["brief_check"].is_object(), "{b}: {reply}");
        for gone in reply["omitted"].as_array().into_iter().flatten() {
            assert!(gone.as_str().is_some_and(|g| g.starts_with("project.")), "{b}: {reply}");
        }
        apply_and_check(&mut s, &mut ed);
    }
}

/// Seen live: a brief that does not build (its why not names the conflicting demands) came back
/// as a plan conflict, which the assistant cannot resolve, and was retried 27 times. It is the
/// brief's arguments that are refused, with the why not kept.
#[test]
fn a_brief_that_does_not_build_is_refused_as_its_arguments_not_as_a_plan_conflict() {
    // a brief that parses and does not build: one storey has no stair down to its basement
    let brief = json!({"typology": "office", "floors": 1, "basements": 1});
    for refine in ["1", "0"] {
        let mut ed = editor();
        ed.model.set_ai_policy(ddc_app::ai::project_tools::POLICY_REFINE, refine).unwrap();
        let (_, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": brief}))]);
        let (text, is_err) = tool_results(&provider).remove(0);
        assert!(is_err, "{text}");
        let v = parsed(&text);
        assert_eq!(v["refused"]["code"], "invalid_args", "{text}");
        assert!(v["error"].as_str().is_some_and(|e| e.contains("needs the stair down")), "{text}");
    }
}

/// A brief the rule critic cannot build is refused just as the plain build refuses it, with
/// its why not; the context lists an applied project with the spec revise_project patches.
#[test]
fn a_refused_brief_keeps_its_why_not_and_the_context_lists_projects() {
    let brief = json!({"typology": "office", "floors": 8, "massing": {"depth": 9},
        "rules": {"corridor.width": {"value": 2.4, "hard": true}, "depth.band": {"value": 6, "hard": true}}});
    let refuse = |ed: &mut Editor| {
        let (_, provider) = run(ed, vec![tool("generate_project", json!({"spec": brief}))]);
        tool_results(&provider).remove(0)
    };
    let on = refuse(&mut editor());
    let mut ed = editor();
    ed.model.set_ai_policy(ddc_app::ai::project_tools::POLICY_REFINE, "0").unwrap();
    let off = refuse(&mut ed);
    assert!(on.1 && off.1, "{on:?} {off:?}");
    assert_eq!(on.0, off.0, "the refined path says what the plain build says");
    assert!(!on.0.contains("does not build"), "{}", on.0);

    let mut ed = editor();
    let spec = json!({"typology": "house", "floors": 2});
    let (mut s, _) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    apply_and_check(&mut s, &mut ed);
    let (_, provider) = run(&mut ed, vec![tool("get_context", json!({}))]);
    let r = tool_results(&provider);
    let projects = &parsed(&r[0].0)["projects"];
    assert_eq!(projects[0]["spec"]["typology"], "house", "{}", r[0].0);
    assert_eq!(projects[0]["spec"], stored_input(&ed), "{}", r[0].0);
}
