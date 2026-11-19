// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The modify commands end to end: each call is a draft, its Apply is one undo step, the model
//! ends exactly where the ribbon tool's own `Model` call leaves a twin of the fixture, and bad
//! arguments come back as coded `invalid_args` naming the argument.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::registry::{self, ErrorCode};
use ddc_ai::ToolOutcome;
use ddc_app::editor::Editor;
use ddc_model::{Model, Spacing, Target};
use e2e_ai_common::*;
use e2e_common::{assert_same, dump_except};
use serde_json::{json, Value as J};

/// Two joined walls and one free wall; the ids are the same in every fixture.
fn fixture() -> (Editor, [i64; 3]) {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let a = add_wall(&mut ed, level, (0.0, 0.0), (10.0, 0.0));
    let b = add_wall(&mut ed, level, (10.0, 0.0), (10.0, 6.0));
    let c = add_wall(&mut ed, level, (0.0, 10.0), (4.0, 10.0));
    (ed, [a, b, c])
}

fn r(id: i64) -> String {
    format!("wall:{id}")
}

fn target(ids: &[i64]) -> J {
    json!({"refs": ids.iter().map(|i| r(*i)).collect::<Vec<_>>()})
}

/// Runs one call, applies it (one undo step, preview equal to the result) and checks the model
/// against a twin changed by `ui`, the call the interactive tool makes. Answers the reply.
fn same_as_ui(name: &str, args: J, ui: impl Fn(&mut Model, [i64; 3])) -> J {
    let (mut ed, ids) = fixture();
    let before = live_hash(&ed.model);
    let (mut s, provider) = run(&mut ed, vec![tool(name, args)]);
    let results = tool_results(&provider);
    assert_eq!(results.len(), 1, "{name}: {results:?}");
    assert!(!results[0].1, "{name}: {}", results[0].0);
    assert_eq!(live_hash(&ed.model), before, "{name}: a draft never touches the model");
    apply_and_check(&mut s, &mut ed);
    let (mut twin, twin_ids) = fixture();
    assert_eq!(ids, twin_ids);
    ui(&mut twin.model, twin_ids);
    assert_same(
        &dump_except(&ed.model, NOT_MODEL),
        &dump_except(&twin.model, NOT_MODEL),
        name,
    );
    parsed(&results[0].0)
}

fn created(reply: &J) -> Vec<String> {
    reply["created"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

#[test]
fn move_matches_the_move_tool() {
    let args = json!({"title": "Move", "target": target(&[3]), "by": [0.0, 2.0]});
    let args = with_ids(args);
    same_as_ui("move_elements", args, |m, [_, _, c]| {
        m.move_elements(&[Target::Wall(c)], (0.0, 2.0)).unwrap()
    });
}

/// Fixture ids are the rowids 1, 2, 3; the scenarios name the third wall by them.
fn with_ids(args: J) -> J {
    let (_, ids) = fixture();
    assert_eq!(ids, [1, 2, 3], "fixture ids moved: update the scenarios");
    args
}

#[test]
fn copy_matches_the_copy_tool_and_names_the_copies() {
    let args = with_ids(json!({"title": "Copy", "target": target(&[3]),
        "offsets": [[0.0, 3.0], [0.0, 6.0]]}));
    let reply = same_as_ui("copy_elements", args, |m, [_, _, c]| {
        m.copy_elements(&[Target::Wall(c)], &[(0.0, 3.0), (0.0, 6.0)]).unwrap();
    });
    let made = created(&reply);
    assert_eq!(made.len(), 2, "{reply}");
    assert!(made.iter().all(|x| x.starts_with("wall:")), "{reply}");
}

#[test]
fn rotate_copy_matches_the_rotate_tool() {
    let args = with_ids(json!({"title": "Turn", "target": target(&[3]),
        "center": [0.0, 10.0], "angle": 90.0, "copy": true}));
    let reply = same_as_ui("rotate_elements", args, |m, [_, _, c]| {
        let a = 90f64.to_radians();
        m.rotate_elements(&[Target::Wall(c)], (0.0, 10.0), a, true).unwrap();
    });
    assert_eq!(created(&reply).len(), 1, "{reply}");
}

#[test]
fn mirror_matches_the_mirror_tool() {
    let args = with_ids(json!({"title": "Mirror", "target": target(&[1, 2]),
        "a": [0.0, -5.0], "b": [10.0, -5.0], "copy": true}));
    same_as_ui("mirror_elements", args, |m, [a, b, _]| {
        let ts = [Target::Wall(a), Target::Wall(b)];
        m.mirror_elements(&ts, (0.0, -5.0), (10.0, -5.0), true).unwrap();
    });
}

#[test]
fn scale_matches_the_scale_tool() {
    let args = with_ids(json!({"title": "Scale", "target": target(&[3]),
        "center": [0.0, 10.0], "factor": 0.5}));
    same_as_ui("scale_elements", args, |m, [_, _, c]| {
        m.scale_elements(&[Target::Wall(c)], (0.0, 10.0), 0.5, false).unwrap();
    });
}

#[test]
fn linear_and_radial_arrays_match_the_array_tool() {
    let args = with_ids(json!({"title": "Row", "target": target(&[3]),
        "mode": "linear", "count": 3, "step": [0.0, 2.0]}));
    let reply = same_as_ui("array_elements", args, |m, [_, _, c]| {
        m.array_linear(&[Target::Wall(c)], 3, Spacing::Step((0.0, 2.0))).unwrap();
    });
    assert_eq!(created(&reply).len(), 2, "{reply}");
    let args = with_ids(json!({"title": "Fit", "target": target(&[3]),
        "mode": "linear", "count": 3, "step": [0.0, 4.0], "fit": true}));
    same_as_ui("array_elements", args, |m, [_, _, c]| {
        m.array_linear(&[Target::Wall(c)], 3, Spacing::Fit((0.0, 4.0))).unwrap();
    });
    let args = with_ids(json!({"title": "Ring", "target": target(&[3]),
        "mode": "radial", "count": 4, "center": [0.0, 10.0], "angle": 360.0}));
    let reply = same_as_ui("array_elements", args, |m, [_, _, c]| {
        let full = std::f64::consts::TAU;
        m.array_radial(&[Target::Wall(c)], 4, (0.0, 10.0), full).unwrap();
    });
    assert_eq!(created(&reply).len(), 3, "{reply}");
}

#[test]
fn offset_and_split_match_their_tools() {
    let args = with_ids(json!({"title": "Offset", "target": target(&[3]),
        "distance": 1.5, "side": "left"}));
    let reply = same_as_ui("offset_wall", args, |m, [_, _, c]| {
        m.offset_wall(c, 1.5, true).unwrap();
    });
    assert_eq!(created(&reply).len(), 1, "{reply}");
    let args = with_ids(json!({"title": "Split", "target": target(&[3]), "at": [1.0, 10.2]}));
    let reply = same_as_ui("split_wall", args, |m, [_, _, c]| {
        m.split_wall_at(c, (1.0, 10.2)).unwrap();
    });
    assert_eq!(created(&reply).len(), 1, "{reply}");
}

#[test]
fn divide_matches_its_tool() {
    let args = with_ids(json!({"title": "Divide", "target": target(&[3]), "parts": 3}));
    let reply = same_as_ui("split_wall", args, |m, [_, _, c]| {
        m.divide_wall(c, 3).unwrap();
    });
    assert_eq!(created(&reply).len(), 2, "{reply}");
}

#[test]
fn pin_matches_the_pin_tool_and_a_second_pin_has_no_effect() {
    let args = with_ids(json!({"title": "Pin", "target": target(&[1, 3]), "pinned": true}));
    same_as_ui("pin_elements", args, |m, [a, _, c]| {
        m.pin(&[("wall", a), ("wall", c)]).unwrap();
    });
    let (mut ed, _) = fixture();
    let before = live_hash(&ed.model);
    let pin = || {
        let a = json!({"title": "Pin", "target": target(&[3]), "pinned": true});
        tool("pin_elements", a)
    };
    let a = json!({"title": "Free", "target": target(&[1]), "pinned": false});
    let unpin = tool("pin_elements", a);
    let (_s, provider) = run(&mut ed, vec![pin(), pin(), unpin]);
    let results = tool_results(&provider);
    assert_eq!(results.len(), 3, "{results:?}");
    assert!(!results[0].1, "{}", results[0].0);
    assert_eq!(code_of(&results[1].0), ErrorCode::NoEffect, "{}", results[1].0);
    assert_eq!(code_of(&results[2].0), ErrorCode::NoEffect, "{}", results[2].0);
    assert_eq!(live_hash(&ed.model), before);
}

#[test]
fn join_matches_the_join_tool_and_switch_and_unjoin_follow_it() {
    let args = with_ids(json!({"title": "Join", "target": target(&[1, 2]), "action": "join"}));
    same_as_ui("join_elements", args, |m, [a, b, _]| {
        assert!(m.join_selected(&[("wall", a), ("wall", b)]).unwrap() > 0);
    });
    let (mut ed, _) = fixture();
    let call = |action: &str| {
        tool("join_elements", json!({"title": action, "target": target(&[1, 2]), "action": action}))
    };
    let (mut s, provider) = run(&mut ed, vec![call("join"), call("switch")]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    apply_and_check(&mut s, &mut ed);
    assert!(!ed.model.joins().unwrap().is_empty(), "joined and switched");
    let (mut s, provider) = run(&mut ed, vec![call("unjoin")]);
    let results = tool_results(&provider);
    assert!(!results[0].1, "{}", results[0].0);
    apply_and_check(&mut s, &mut ed);
    assert!(ed.model.joins().unwrap().is_empty(), "unjoined: none left");
    let (mut ed, _) = fixture();
    let before = live_hash(&ed.model);
    let (_s, provider) = run(&mut ed, vec![call("unjoin")]);
    let results = tool_results(&provider);
    assert!(results[0].1, "nothing joined: refused, {}", results[0].0);
    assert_eq!(live_hash(&ed.model), before);
}

fn code_of(text: &str) -> ErrorCode {
    assert!(text.starts_with("{\"untrusted\":\"data\""), "{text}");
    registry::error_of(&ToolOutcome::error(text)).expect("an error").code
}

fn path_of(text: &str) -> Option<String> {
    registry::error_of(&ToolOutcome::error(text)).and_then(|e| e.path)
}

/// Every command with arguments the schema lets through but the handler refuses: a coded
/// `invalid_args` naming the argument, no item, the model untouched.
#[test]
fn bad_arguments_are_coded_and_name_the_argument() {
    let t = target(&[3]);
    let two = target(&[1, 3]);
    let cases: Vec<(&str, J, &str)> = vec![
        ("move_elements", json!({"by": [1.0]}), "args.by"),
        ("move_elements", json!({"by": [0.0, 0.0]}), "args.by"),
        ("copy_elements", json!({"offsets": []}), "args.offsets"),
        ("copy_elements", json!({"offsets": [[1.0, 0.0], [0.0]]}), "args.offsets[1]"),
        ("rotate_elements", json!({"center": [0.0, 0.0], "angle": 360.0}), "args.angle"),
        ("rotate_elements", json!({"center": [0.0], "angle": 30.0}), "args.center"),
        ("mirror_elements", json!({"a": [1.0, 1.0], "b": [1.0, 1.0]}), "args.b"),
        ("scale_elements", json!({"center": [0.0, 0.0], "factor": 0.0}), "args.factor"),
        ("array_elements", json!({"mode": "linear", "count": 3, "step": [0.0, 0.0]}), "args.step"),
        ("array_elements", json!({"mode": "radial", "count": 3, "angle": 30.0}), "args.center"),
        ("array_elements", json!({"mode": "linear", "count": 1, "step": [1.0, 0.0]}), "args.count"),
        ("offset_wall", json!({"distance": -1.0, "side": "left"}), "args.distance"),
        ("split_wall", json!({"at": [1.0, 10.0]}), "args.target"),
        ("split_wall", json!({"at": [1.0, 10.0], "parts": 2}), "args.parts"),
        ("join_elements", json!({"action": "join"}), "args.target"),
    ];
    for (name, extra, path) in cases {
        let (mut ed, _) = fixture();
        let before = live_hash(&ed.model);
        let mut args = json!({"title": "Bad", "target": t.clone()});
        if name == "split_wall" {
            args["target"] = two.clone();
        }
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        let (s, provider) = run(&mut ed, vec![tool(name, args.clone())]);
        let results = tool_results(&provider);
        assert!(results[0].1, "{name} {args}: {results:?}");
        assert_eq!(code_of(&results[0].0), ErrorCode::InvalidArgs, "{name}: {}", results[0].0);
        assert_eq!(path_of(&results[0].0).as_deref(), Some(path), "{name}: {}", results[0].0);
        assert!(s.plan().is_none_or(|p| p.items.is_empty()), "{name}: no item stays");
        assert_eq!(live_hash(&ed.model), before, "{name}");
    }
}

/// A wrong argument type never reaches the handler: the gate codes it.
#[test]
fn schema_violations_are_refused_by_the_gate() {
    for (name, args) in [
        ("move_elements", json!({"title": "x", "target": target(&[3]), "by": "north"})),
        ("pin_elements", json!({"title": "x", "target": target(&[3])})),
        ("array_elements", json!({"title": "x", "target": target(&[3]), "mode": "spiral",
            "count": 3})),
    ] {
        let (mut ed, _) = fixture();
        let (_s, provider) = run(&mut ed, vec![tool(name, args)]);
        let results = tool_results(&provider);
        assert_eq!(code_of(&results[0].0), ErrorCode::InvalidArgs, "{name}: {}", results[0].0);
    }
}
