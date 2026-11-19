// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The chat builds geometry with a sandboxed script: a draft item until Apply, one undo step,
//! a recipe stored with the elements, and a rerun with new params that touches only the
//! recipe's own elements. The gate: a steel sphere of about 4500 parts in release.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::{Script, ScriptedProvider};
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::{json, Value as J};
use std::time::Instant;

const FRAME: &str = "\
local n = param(\"bays\", 4)
local h = param(\"height\", \"3 m\")
local post = profile.shs(mm(200), mm(10))
for i = 0, n do
  part(\"post\", extrude(post, vec3(i * 4, 0, 0), vec3(i * 4, 0, h)), \"steel\")
end
for i = 0, n - 1 do
  part(\"beam\", extrude(profile.named(\"IPE 200\"), vec3(i * 4, 0, h), vec3(i * 4 + 4, 0, h)))
end
measure(vec3(0, 0, 0), vec3(n * 4, 0, 0))
";

const SPHERE: &str = include_str!("../../script/data/steel_sphere.lua");

fn run_script(script: &str, params: J) -> Script {
    tool(
        "run_script",
        json!({"title": "Frame", "script": script, "params": params}),
    )
}

fn rerun(recipe: i64, params: J) -> Script {
    tool(
        "rerun_recipe",
        json!({"title": "Frame again", "recipe": recipe, "params": params}),
    )
}

/// Every row of a table, as text: the invariant outside a recipe's scope.
fn rows(ed: &Editor, table: &str) -> Vec<String> {
    let mut st = ed
        .model
        .sql()
        .prepare(&format!("SELECT * FROM {table} ORDER BY id"))
        .unwrap();
    let n = st.column_count();
    st.query_map([], |r| {
        Ok((0..n)
            .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
            .collect::<Vec<_>>()
            .join("|"))
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

fn with_wall() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let l = add_level(&mut ed, "L1", 0.0);
    add_wall(&mut ed, l, (0.0, 5.0), (10.0, 5.0));
    ed
}

fn ok(provider: &ScriptedProvider) -> Vec<J> {
    let results = tool_results(provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    results.iter().map(|r| parsed(&r.0)).collect()
}

#[test]
fn a_script_is_a_draft_and_its_rerun_touches_only_its_own_parts() {
    let mut ed = with_wall();
    let walls = rows(&ed, "wall");
    let before = live_hash(&ed.model);
    let (mut s, provider) = run(&mut ed, vec![run_script(FRAME, json!({}))]);
    let r = &ok(&provider)[0];
    assert_eq!(r["changes"]["created"], json!(9), "{r}");
    assert_eq!(r["params"]["bays"]["value"], json!(4));
    assert_eq!(r["params"]["height"]["unit"], json!("m"));
    assert_eq!(r["measures"][0]["value"], json!(16.0));
    assert_eq!(
        live_hash(&ed.model),
        before,
        "0 writes to the live model before Apply"
    );
    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "solid"), 9);
    let recipes = ed.model.recipes().unwrap();
    assert_eq!(recipes.len(), 1);
    let id = recipes[0].id;
    assert_eq!(ed.model.recipe_parts(id).unwrap().len(), 9);
    drop(s);

    // fewer bays: the last post and beam go, the rest stay as they are
    let solids = rows(&ed, "solid");
    let (mut s, provider) = run(&mut ed, vec![rerun(id, json!({"bays": 3}))]);
    let r = &ok(&provider)[0];
    assert_eq!(r["diff"]["kept"], json!(7), "{r}");
    assert_eq!(r["diff"]["deleted"], json!(["beam#4", "post#5"]));
    assert_eq!(r["changes"]["deleted"], json!(2));
    assert_eq!(rows(&ed, "solid"), solids, "nothing written before Apply");
    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "solid"), 7);
    assert_eq!(rows(&ed, "wall"), walls, "outside the recipe nothing moved");
    let kept: Vec<String> = rows(&ed, "solid");
    assert!(
        solids.iter().take(4).all(|x| kept.contains(x)),
        "kept parts are the same rows"
    );
    assert!(ed
        .model
        .recipe(id)
        .unwrap()
        .unwrap()
        .params_json
        .contains("\"bays\":3"));
    drop(s);

    // a taller frame: every part reshaped in place, ids kept
    let ids = rows(&ed, "solid").len();
    let (mut s, provider) = run(&mut ed, vec![rerun(id, json!({"height": "3.5 m"}))]);
    let r = &ok(&provider)[0];
    assert_eq!(
        r["changes"],
        json!({"created": 0, "changed": 7, "deleted": 0}),
        "{r}"
    );
    apply_and_check(&mut s, &mut ed);
    assert_eq!(rows(&ed, "solid").len(), ids);
    assert_eq!(ed.model.recipe_parts(id).unwrap().len(), 7);
    assert_eq!(rows(&ed, "wall"), walls);
}

#[test]
fn script_errors_come_back_with_line_column_and_hint() {
    let mut ed = with_wall();
    let bad = "local a = 1\nlocal f = extrud(profile.rect(1, 1), vec3(0,0,0), vec3(1,0,0))\n";
    let io = "local f = io.open(\"x\")\n";
    let spin = "while true do end\n";
    let (s, provider) = run(
        &mut ed,
        vec![
            run_script(bad, json!({})),
            run_script(io, json!({})),
            run_script(spin, json!({})),
        ],
    );
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| r.1), "{results:?}");
    assert!(results[0].0.contains("line 2, col 11"), "{}", results[0].0);
    assert!(
        results[0].0.contains("extrude"),
        "the hint names the stdlib function"
    );
    assert!(results[1].0.contains("line 1"), "{}", results[1].0);
    assert!(results[2].0.contains("200000"), "{}", results[2].0);
    assert!(s.plan().is_none(), "a failed script leaves no item");
}

#[test]
fn an_unknown_param_is_refused() {
    let mut ed = with_wall();
    let (s, provider) = run(&mut ed, vec![run_script(FRAME, json!({"bayz": 3}))]);
    let results = tool_results(&provider);
    assert!(results[0].1 && results[0].0.contains("bays"), "{results:?}");
    assert!(s.plan().is_none());
}

const NOTCHED: &str = "
local h = param(\"height\", \"2 m\")
local bar = part(\"bar\", extrude(profile.rect(0.4, 0.4), vec3(0, 0, 0), vec3(0, 0, h)))
boolean(\"cut\", bar, extrude(profile.rect(0.1, 0.1), vec3(0, 0, 0.5), vec3(0, 0, 1.5)))
";

#[test]
fn a_boolean_cut_applies_as_linked_masses_in_one_undo_step() {
    let mut ed = with_wall();
    let before = live_hash(&ed.model);
    let (mut s, provider) = run(&mut ed, vec![run_script(NOTCHED, json!({}))]);
    let r = &ok(&provider)[0];
    assert_eq!(r["changes"]["created"], json!(1), "{r}");
    assert_eq!(live_hash(&ed.model), before, "0 writes before Apply");
    apply_and_check(&mut s, &mut ed);
    // the part and the tool are masses and the linked result follows them; no solid
    assert_eq!(count(&ed.model, "solid"), 0);
    assert_eq!(count(&ed.model, "mass"), 3);
    let id = ed.model.recipes().unwrap()[0].id;
    let parts = ed.model.recipe_parts(id).unwrap();
    assert_eq!((parts.len(), parts[0].tbl.as_str()), (1, "mass"));
    ed.undo();
    assert_eq!(count(&ed.model, "mass"), 0);
    assert_eq!(live_hash(&ed.model), before, "one undo takes it all back");
    drop(s);
}

#[test]
fn a_rerun_that_would_change_a_boolean_part_is_refused_with_a_reason() {
    let mut ed = with_wall();
    let (mut s, provider) = run(&mut ed, vec![run_script(NOTCHED, json!({}))]);
    ok(&provider);
    apply_and_check(&mut s, &mut ed);
    drop(s);
    let id = ed.model.recipes().unwrap()[0].id;
    let masses = rows(&ed, "mass");
    let (s, provider) = run(&mut ed, vec![rerun(id, json!({"height": "3 m"}))]);
    let results = tool_results(&provider);
    assert!(results[0].1, "{results:?}");
    assert!(results[0].0.contains("rebuild booleans"), "{}", results[0].0);
    assert!(s.plan().is_none());
    assert_eq!(rows(&ed, "mass"), masses, "nothing moved");
}

#[test]
fn a_rerun_checks_the_stored_hash_params_and_level() {
    let mut ed = with_wall();
    let (mut s, provider) = run(&mut ed, vec![run_script(FRAME, json!({}))]);
    ok(&provider);
    apply_and_check(&mut s, &mut ed);
    drop(s);
    let id = ed.model.recipes().unwrap()[0].id;

    // a level added below does not move the recipe: it still builds on its own level
    add_level(&mut ed, "Basement", -3.0);
    let (mut s, provider) = run(&mut ed, vec![rerun(id, json!({"bays": 3}))]);
    ok(&provider);
    apply_and_check(&mut s, &mut ed);
    drop(s);
    let levels: i64 = ed
        .model
        .sql()
        .query_row("SELECT count(DISTINCT level) FROM solid", [], |r| r.get(0))
        .unwrap();
    assert_eq!(levels, 1, "every part stayed on the recipe's level");

    // a stored hash that the script no longer reproduces is refused by name
    ed.model
        .sql()
        .execute("UPDATE recipe SET hash='0000' WHERE id=?1", [id])
        .unwrap();
    let (s, provider) = run(&mut ed, vec![rerun(id, json!({"bays": 2}))]);
    let results = tool_results(&provider);
    assert!(results[0].1 && results[0].0.contains("no longer builds"), "{results:?}");
    assert!(s.plan().is_none());
    drop(s);

    // params that cannot be read are an error, never an empty set
    ed.model
        .sql()
        .execute("UPDATE recipe SET params_json='{oops' WHERE id=?1", [id])
        .unwrap();
    let (s, provider) = run(&mut ed, vec![rerun(id, json!({"bays": 2}))]);
    let results = tool_results(&provider);
    assert!(results[0].1 && results[0].0.contains("not valid JSON"), "{results:?}");
    assert!(s.plan().is_none());
}

/// Release gate: build the sphere, apply it, change the radius, apply again; each <= 10 s.
#[test]
#[cfg_attr(debug_assertions, ignore = "timing gate: run with --release")]
fn gate_steel_sphere_builds_and_rebuilds_in_ten_seconds() {
    let mut ed = with_wall();
    let t = Instant::now();
    let (mut s, provider) = run(&mut ed, vec![run_script(SPHERE, json!({}))]);
    let r = &ok(&provider)[0];
    let built = t.elapsed();
    let parts = r["changes"]["created"].as_u64().unwrap();
    if !s.can_apply() {
        s.allow_large(true);
        assert!(s.wait_idle(&mut ed, WAIT));
    }
    let t2 = Instant::now();
    s.apply(&mut ed).unwrap();
    assert!(s.wait_idle(&mut ed, WAIT));
    let first = built + t2.elapsed();
    assert_eq!(count(&ed.model, "solid") as u64, parts);
    drop(s);

    let id = ed.model.recipes().unwrap()[0].id;
    let t = Instant::now();
    let (mut s, provider) = run(&mut ed, vec![rerun(id, json!({"radius": "14 m"}))]);
    let r2 = &ok(&provider)[0];
    if !s.can_apply() {
        s.allow_large(true);
        assert!(s.wait_idle(&mut ed, WAIT));
    }
    s.apply(&mut ed).unwrap();
    assert!(s.wait_idle(&mut ed, WAIT));
    let second = t.elapsed();
    println!(
        "GATE sphere parts={parts} build+apply={:.2}s script_cpu={}ms rerun={:.2}s \
         diff={} solids={}",
        first.as_secs_f64(),
        r["stats"]["cpu_ms"],
        second.as_secs_f64(),
        r2["diff"],
        count(&ed.model, "solid")
    );
    assert!((4300..=4700).contains(&parts), "{parts}");
    assert!(first.as_secs_f64() <= 10.0, "build {first:?}");
    assert!(second.as_secs_f64() <= 10.0, "rebuild {second:?}");
}

/// The shape of the H04 stadium script: an oval ring of truss chords on masts, parts by loops.
const STADIUM_ROOF: &str = "\
local n = param(\"bays\", 24)
local rx, ry, h = 90, 70, 38
local chord = profile.named(\"IPE 200\")
local mast = profile.shs(mm(400), mm(20))
for i = 0, n - 1 do
  local a0, a1 = 2 * math.pi * i / n, 2 * math.pi * (i + 1) / n
  local p0 = vec3(rx * math.cos(a0), ry * math.sin(a0), h)
  local p1 = vec3(rx * math.cos(a1), ry * math.sin(a1), h)
  part(\"ring\", extrude(chord, p0, p1), \"steel\")
  part(\"mast\", extrude(mast, vec3(1.2 * p0.x, 1.2 * p0.y, 0), vec3(1.2 * p0.x, 1.2 * p0.y, h + 12)), \"steel\")
end
";

fn run_stadium(ed: &mut Editor) {
    let (mut s, provider) = run(ed, vec![run_script(STADIUM_ROOF, json!({}))]);
    let r = &ok(&provider)[0];
    assert_eq!(r["changes"]["created"], json!(48), "{r}");
    apply_and_check(&mut s, ed);
    assert_eq!(ed.model.recipes().unwrap().len(), 1);
}

/// `meta.project_uid`, `phase_current` and `schema_version` are INTEGER in files the app made:
/// run_script read the uid as text and failed with "Invalid column type Integer at index: 0,
/// name: value" on every call (H04, 12 failures).
#[test]
fn run_script_works_on_a_file_whose_meta_values_are_integers() {
    let mut ed = Editor::new_in_memory().unwrap();
    for k in ["project_uid", "schema_version"] {
        let t: String = ed.model.sql().query_row("SELECT typeof(value) FROM meta WHERE key=?1", [k], |r| r.get(0)).unwrap();
        assert_eq!(t, "integer", "{k}");
    }
    run_stadium(&mut ed);
}

/// The same on the real H04 model: set `DDC_H04_MODEL` to its `model.ddc`.
#[test]
fn run_script_works_on_the_h04_model_file() {
    let Ok(path) = std::env::var("DDC_H04_MODEL") else { return };
    let tmp = std::env::temp_dir().join("h04_model_copy.ddc");
    std::fs::copy(&path, &tmp).unwrap();
    let mut ed = Editor::new_in_memory().unwrap();
    ed.replace_model(ddc_model::Model::open(tmp.to_str().unwrap()).unwrap()).unwrap();
    run_stadium(&mut ed);
}
