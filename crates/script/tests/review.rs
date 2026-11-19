//! Correctness of what a script builds: booleans the model accepts, honest param bounds, a
//! run-wide copy budget, and rerun plans that never ask the model for the impossible.

use ddc_ops::{Actor, Cat, GeoOp, Op, Ref};
use ddc_script::{rerun_envelope, ErrorKind, GeoScript, Link, RunOpts, Sandbox, ScriptError};
use ddc_script::ScriptRun;
use serde_json::json;

fn run_with(src: &str, params: serde_json::Value) -> Result<ScriptRun, ScriptError> {
    Sandbox.run(src, &RunOpts::new(Ref::name(Cat::Level, "Level 1"), params))
}

fn run(src: &str) -> Result<ScriptRun, ScriptError> {
    run_with(src, json!({}))
}

fn ops(r: &ScriptRun) -> Vec<&Op> {
    r.env.groups.iter().flat_map(|g| g.ops.iter()).collect()
}

const PAD: &str = "\npart('_pad', extrude(profile.rect(0.1, 0.1), vec3(0,0,0), vec3(1,0,0)))";

#[test]
fn boolean_turns_the_part_into_a_mass_and_keeps_the_tool() {
    let r = run(
        "local a = part('a', extrude(profile.rect(0.4, 0.4), vec3(0,0,0), vec3(0,0,2)))\n\
         boolean('cut', a, extrude(profile.rect(0.1, 0.1), vec3(0,0,0.5), vec3(0,0,1.5)))",
    )
    .unwrap();
    let o = ops(&r);
    assert_eq!(o.len(), 2);
    assert!(matches!(o[0], Op::Geometry(GeoOp::Mass { name: Some(n), .. }) if n == "a"), "{o:?}");
    assert!(matches!(o[1], Op::Geometry(GeoOp::Boolean { keep_tool: true, .. })), "{o:?}");
    assert!(r.env.check().is_empty());
    // all three kinds, and a second boolean on the same mass
    for k in ["union", "intersect"] {
        run(&format!(
            "local a = part('a', extrude(profile.rect(0.4, 0.4), vec3(0,0,0), vec3(0,0,2)))\n\
             boolean('{k}', a, extrude(profile.rect(0.1, 0.1), vec3(0,0,0.5), vec3(0,0,1.5)))\n\
             boolean('cut', a, extrude(profile.rect(0.1, 0.1), vec3(0,0,1.6), vec3(0,0,1.9)))"
        ))
        .unwrap();
    }
}

#[test]
fn boolean_refuses_what_the_model_would_refuse_with_the_line() {
    let e = run(
        "local p = profile.rect(0.1, 0.1)\n\
         local a = part('a', sweep({vec3(0,0,0), vec3(0,0,2)}, p))\n\
         boolean('cut', a, extrude(p, vec3(0,0,0.5), vec3(0,0,1)))",
    )
    .unwrap_err();
    assert_eq!((e.kind, e.line), (ErrorKind::Runtime, 3), "{e}");
    assert!(e.message.starts_with("boolean:") && e.message.contains("extrusion"), "{e}");
    let e = run(
        "local p = profile.rect(0.1, 0.1)\n\
         local a = part('a', extrude(p, vec3(0,0,0), vec3(0,0,2)), 'steel')\n\
         boolean('cut', a, extrude(p, vec3(0,0,0.5), vec3(0,0,1)))",
    )
    .unwrap_err();
    assert!(e.message.contains("material"), "{e}");
    let e = run(
        "local p = profile.rect(0.1, 0.1)\n\
         local a = part('a', extrude(p, vec3(0,0,0), vec3(0,0,2)))\n\
         boolean('cut', a, sweep({vec3(0,0,0), vec3(0,0,1)}, p))",
    )
    .unwrap_err();
    assert!(e.message.contains("second argument must be an extrusion"), "{e}");
}

#[test]
fn param_bounds_of_a_unit_value_need_units() {
    let src = |b: &str| format!("local w = param('w', '2 m', {b})\n{}", &PAD[1..]);
    run(&src("{min = '1 m', max = '5 m'}")).unwrap();
    run(&src("{min = '500 mm'}")).unwrap();
    let e = run(&src("{min = 1}")).unwrap_err();
    assert_eq!((e.kind, e.line), (ErrorKind::Param, 1), "{e}");
    assert!(e.message.contains("`min` needs one too"), "{e}");
    let e = run(&src("{max = 5.5}")).unwrap_err();
    assert!(e.message.contains("`max` needs one too"), "{e}");
    // an angle too
    let a = |b: &str| format!("local t = param('t', '30 deg', {b})\n{}", &PAD[1..]);
    run(&a("{min = '10 deg', max = '80 deg'}")).unwrap();
    assert!(run(&a("{max = 1.2}")).unwrap_err().message.contains("needs one too"));
    // plain numbers keep plain bounds
    run(&format!("local n = param('n', 4, {{min = 1, max = 9}})\n{}", &PAD[1..])).unwrap();
}

#[test]
fn pattern_copies_are_counted_over_the_whole_run() {
    let call = "c = pattern_linear(f, 2, vec3(0, 1, 0))\n";
    let head = "local pts = {}\nfor i = 1, 50000 do pts[i] = {math.cos(i), math.sin(i)} end\n\
                local f = extrude(profile.polygon(pts), vec3(0,0,0), vec3(1,0,0))\nlocal c\n";
    let mut o = RunOpts::new(Ref::name(Cat::Level, "Level 1"), json!({}));
    o.limits.ops = u64::MAX;
    o.limits.cpu = std::time::Duration::from_secs(30);
    // each call is 100 000 points: well under the budget alone, over it as a sum
    let one = format!("{head}{call}{}", &PAD[1..]);
    Sandbox.run(&one, &o).unwrap();
    let many = format!("{head}for i = 1, 60 do {call}end\n{}", &PAD[1..]);
    let e = Sandbox.run(&many, &o).unwrap_err();
    assert!(e.message.starts_with("pattern:") && e.message.contains("already"), "{e}");
    assert!(e.message.contains("per run"), "{e}");
}

fn links_for(r: &ScriptRun, cat: Cat) -> Vec<Link> {
    r.parts
        .iter()
        .enumerate()
        .map(|(i, p)| Link { key: p.key.clone(), cat, id: 100 + i as i64 })
        .collect()
}

fn plan(old: &ScriptRun, new: &ScriptRun, cat: Cat) -> Result<ddc_script::RerunPlan, String> {
    let actor = Actor::Script { name: "t".into() };
    rerun_envelope(old, new, &links_for(old, cat), actor, "t")
}

#[test]
fn rerun_replaces_a_sweep_that_becomes_an_extrusion() {
    let src = "local p = profile.rect(0.1, 0.1)\nlocal long = param('long', true)\n\
               if long then\n  part('m', sweep({vec3(0,0,0), vec3(0,0,2)}, p))\nelse\n\
               part('m', extrude(p, vec3(0,0,0), vec3(0,0,2)))\nend";
    let old = run_with(src, json!({})).unwrap();
    let new = run_with(src, json!({"long": false})).unwrap();
    let p = plan(&old, &new, Cat::Solid).unwrap();
    assert!(p.reshaped.is_empty(), "{p:?}");
    assert_eq!(p.replaced, vec!["m".to_string()]);
    // extrusion to extrusion is still a reshape in place
    let a = run("part('m', extrude(profile.rect(0.1, 0.1), vec3(0,0,0), vec3(0,0,2)))").unwrap();
    let b = run("part('m', extrude(profile.rect(0.1, 0.1), vec3(0,0,0), vec3(0,0,3)))").unwrap();
    let p = plan(&a, &b, Cat::Solid).unwrap();
    assert_eq!((p.reshaped.len(), p.replaced.len()), (1, 0));
}

#[test]
fn rerun_refuses_to_change_a_boolean_mass_and_keeps_an_unchanged_one() {
    let src = "local h = param('h', '2 m')\n\
               local a = part('a', extrude(profile.rect(0.4, 0.4), vec3(0,0,0), vec3(0,0,h)))\n\
               boolean('cut', a, extrude(profile.rect(0.1, 0.1), vec3(0,0,0.5), vec3(0,0,1.5)))";
    let old = run_with(src, json!({})).unwrap();
    let same = run_with(src, json!({})).unwrap();
    let p = plan(&old, &same, Cat::Mass).unwrap();
    assert!(p.env.is_none() && p.kept == 1, "{p:?}");
    let taller = run_with(src, json!({"h": "3 m"})).unwrap();
    let e = plan(&old, &taller, Cat::Mass).unwrap_err();
    assert!(e.contains("rebuild booleans") && e.contains("`a`"), "{e}");
}
