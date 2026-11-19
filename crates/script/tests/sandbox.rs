//! Sandbox contract: determinism, limits, no IO, errors with position and hint, params,
//! keys, rerun scope.

use std::time::{Duration, Instant};

use ddc_ops::{Actor, Cat, GeoOp, Op, Ref, Targets};
use ddc_script::{
    rerun_envelope, ErrorKind, GeoScript, LimitKind, Link, RunOpts, Sandbox, ScriptError, ScriptRun,
};
use serde_json::json;

const SPHERE: &str = include_str!("../data/steel_sphere.lua");

const BAYS: &str = r#"
local n = param("bays", 4)
local span = param("span", "6 m")
local h = param("height", "3 m")
local p = profile.named("HEA 200")
for i = 0, n do
  local x = i * span
  part("post", extrude(p, vec3(x, 0, 0), vec3(x, 0, h)))
end
for i = 0, n - 1 do
  part("beam", extrude(p, vec3(i * span, 0, h), vec3((i + 1) * span, 0, h)))
end
"#;

fn opts(params: serde_json::Value) -> RunOpts {
    RunOpts::new(Ref::name(Cat::Level, "Level 1"), params)
}

fn run(src: &str) -> Result<ScriptRun, ScriptError> {
    Sandbox.run(src, &opts(json!({})))
}

fn fail(src: &str) -> ScriptError {
    run(src).expect_err("script must fail")
}

#[test]
fn same_script_and_params_give_the_same_hash() {
    let a = Sandbox.run(SPHERE, &opts(json!({}))).unwrap();
    let b = Sandbox.run(SPHERE, &opts(json!({}))).unwrap();
    assert_eq!(a.hash, b.hash);
    assert_eq!(a.env, b.env);
    assert_eq!(a.parts, b.parts);
    let c = Sandbox
        .run(SPHERE, &opts(json!({"radius": "13 m"})))
        .unwrap();
    assert_ne!(a.hash, c.hash);
    // the table order of `pairs` does not leak the interpreter's hash seed
    let src = "local t = {zeta=1, alpha=2, mid=3, [5]=4, [1]=5}\nlocal s = ''\n\
               for k, v in pairs(t) do s = s .. tostring(k) end\nprint(s)\n\
               part('p', extrude(profile.rect(0.1, 0.1), vec3(0,0,0), vec3(1,0,0)))";
    assert_eq!(run(src).unwrap().log, vec!["15alphamidzeta".to_string()]);
}

#[test]
fn sphere_builds_about_4500_parts_within_limits() {
    let t = Instant::now();
    let r = Sandbox.run(SPHERE, &opts(json!({}))).unwrap();
    let ms = t.elapsed().as_secs_f64() * 1e3;
    assert!(
        (4300..=4700).contains(&r.parts.len()),
        "{} parts",
        r.parts.len()
    );
    assert!(r.env.check().is_empty(), "{:?}", r.env.check().first());
    assert!(r.constraints >= 50 && r.measures.len() == 1);
    assert!(r.stats.ops < 200_000, "{} ops", r.stats.ops);
    let keys: std::collections::HashSet<&str> = r.parts.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys.len(), r.parts.len(), "keys are unique");
    assert!(keys.contains("ring 1/tube") && keys.contains("ring 1/tube#2"));
    assert!(keys.contains("stair/tread#3"));
    println!(
        "sphere script: {} parts, {} ops, {:.0} ms wall, {:.0} ms cpu, {} KB peak",
        r.parts.len(),
        r.stats.ops,
        ms,
        r.stats.cpu_ms,
        r.stats.memory_peak >> 10
    );
}

#[test]
fn a_loop_that_never_ends_is_stopped() {
    let e = fail("local x = 0\nwhile true do\n  x = x + 1\nend");
    assert_eq!(e.kind, ErrorKind::Limit(LimitKind::Ops));
    assert!(e.line >= 2 && e.line <= 3, "{e}");
    // on one line, without a counted line per turn: CPU time stops it
    let mut o = opts(json!({}));
    o.limits.ops = u64::MAX;
    o.limits.cpu = Duration::from_millis(300);
    let t = Instant::now();
    let e = Sandbox.run("while true do end", &o).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Limit(LimitKind::Cpu));
    assert!(t.elapsed() < Duration::from_secs(5));
    // pcall cannot swallow a limit
    let e = fail("while true do pcall(function() while true do end end) end");
    assert!(matches!(e.kind, ErrorKind::Limit(_)), "{e}");
}

#[test]
fn memory_is_capped() {
    let mut o = opts(json!({}));
    o.limits.ops = u64::MAX;
    o.limits.cpu = Duration::from_secs(30);
    o.limits.memory = 16 << 20;
    let e = Sandbox
        .run("local t = {}\nfor i = 1, 1e9 do t[i] = 'x' .. i end", &o)
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Limit(LimitKind::Memory), "{e}");
}

#[test]
fn files_network_clock_and_code_loading_are_absent() {
    for src in [
        "io.open('x.txt', 'w')",
        "os.time()",
        "os.execute('dir')",
        "require('socket')",
        "load('return 1')()",
        "dofile('x.lua')",
        "loadfile('x.lua')",
        "math.random()",
        "math.randomseed(1)",
        "string.dump(print)",
        "collectgarbage()",
        "next({})",
        "debug.getinfo(1)",
        "package.loadlib('x', 'y')",
        "coroutine.create(print)",
        "string.format('%p', {})",
    ] {
        let e = fail(src);
        assert_eq!(e.kind, ErrorKind::Runtime, "{src}: {e}");
        assert_eq!(e.line, 1, "{src}: {e}");
    }
    let r = run("print(tostring({}), tostring(print))\n\
                 part('p', extrude(profile.rect(0.1, 0.1), vec3(0,0,0), vec3(1,0,0)))")
    .unwrap();
    assert_eq!(r.log, vec!["table\tfunction".to_string()]);
}

#[test]
fn errors_carry_line_column_and_a_hint() {
    let e = fail("local p = profile.rect(0.2, 0.2)\nlocal f = extrud(p, vec3(), vec3(1, 0, 0))");
    assert_eq!((e.kind, e.line, e.col), (ErrorKind::Runtime, 2, 11), "{e}");
    assert!(e.hint.contains("extrude"), "{e}");

    let e = fail("for i = 1, 3\n  print(i)\nend");
    assert_eq!(e.kind, ErrorKind::Syntax);
    assert_eq!(e.line, 2, "{e}");
    assert!(e.hint.contains("do"), "{e}");

    let e = fail("local a = 1\n\nlocal x = len(1200)");
    assert_eq!((e.line, e.col), (3, 11), "{e}");
    assert!(
        e.message.starts_with("len:") && e.hint.contains("mm"),
        "{e}"
    );

    let e = fail(
        "local p = profile.rect(0.2, 0.2)\n  part('x', extrude(p, vec3(), vec3(0.001, 0, 0)))",
    );
    assert_eq!(e.line, 2, "{e}");
    assert!(e.message.contains("5 mm"), "{e}");

    let e = fail(
        "local p = profile.rect(0.2, 0.2)\nlocal a = part('a', extrude(p, vec3(), vec3(1,0,0)))\n\
                  constrain('coincident', a.b, vec3(1, 0.01, 0))",
    );
    assert_eq!(e.line, 3, "{e}");
    assert!(e.message.contains("10.0 mm"), "{e}");

    let e = fail("assert(1 + 1 == 3, 'math is broken')");
    assert_eq!(e.kind, ErrorKind::Assert);
    assert!(
        e.to_string()
            .starts_with("line 1, col 1: assert: math is broken"),
        "{e}"
    );

    let e = fail("local p = profile.named('HEA 999')");
    assert!(e.message.contains("HEA 200"), "catalog names listed: {e}");
    assert!(!e.message.contains("Lua") && !e.hint.contains("Lua"));
}

#[test]
fn element_cap_needs_allow_many() {
    let src = "for i = 1, 30 do \
               part('p', extrude(profile.rect(0.1, 0.1), vec3(i, 0, 0), vec3(i, 1, 0))) end";
    let mut o = opts(json!({}));
    o.limits.elements = 20;
    let e = Sandbox.run(src, &o).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Limit(LimitKind::Elements));
    assert!(e.hint.contains("allow"), "{e}");
    o.limits.allow_many = true;
    assert_eq!(Sandbox.run(src, &o).unwrap().parts.len(), 30);
}

#[test]
fn params_are_declared_typed_and_ranged() {
    let r = Sandbox
        .run(
            BAYS,
            &opts(json!({"bays": 2, "span": {"v": 5000, "unit": "mm"}})),
        )
        .unwrap();
    assert_eq!(r.parts.len(), 5);
    let span = r.params.iter().find(|p| p.name == "span").unwrap();
    assert_eq!((span.unit, span.value.as_f64()), (Some("m"), Some(5.0)));
    assert_eq!(span.default, json!("6 m"));

    let e = Sandbox
        .run(BAYS, &opts(json!({"width": "3 m"})))
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Param);
    assert!(e.hint.contains("bays") && e.hint.contains("span"), "{e}");
    let e = Sandbox.run(BAYS, &opts(json!({"span": 6}))).unwrap_err();
    assert_eq!((e.kind, e.line), (ErrorKind::Param, 3), "{e}");
    let e = Sandbox.run(BAYS, &opts(json!({"bays": 2.5}))).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Param, "{e}");
    let e = Sandbox
        .run(SPHERE, &opts(json!({"radius": "100 m"})))
        .unwrap_err();
    assert!(e.message.contains("maximum"), "{e}");
}

#[test]
fn groups_components_patterns_and_keys() {
    let src = r#"
local p = profile.rect(0.1, 0.2)
local leg = define_component("leg", {
  {name = "post", form = extrude(p, vec3(0, 0, 0), vec3(0, 0, 1))},
  {name = "foot", form = extrude(p, vec3(-0.2, 0, 0), vec3(0.2, 0, 0)), material = "S235"},
})
for i = 1, 3 do place(leg, vec3(i * 2, 0, 0), deg(90)) end
local ring = pattern_radial(extrude(p, vec3(5, 0, 0), vec3(6, 0, 0)), 4, deg(360))
group("spokes", parts("spoke", ring))
local row = pattern_linear(extrude(p, vec3(0, 5, 0), vec3(1, 5, 0)), 3, vec3(2, 0, 0))
local g = group("row", function() for _, f in ipairs(row) do part("bar", f) end end)
assert(g.count == 3)
assert(math.abs(measure(ring[2]) - 1) < 1e-12)
"#;
    let r = run(src).unwrap();
    let keys: Vec<&str> = r.parts.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "leg/post",
            "leg/foot",
            "leg/post#2",
            "leg/foot#2",
            "leg/post#3",
            "leg/foot#3",
            "spokes/spoke",
            "spokes/spoke#2",
            "spokes/spoke#3",
            "spokes/spoke#4",
            "row/bar",
            "row/bar#2",
            "row/bar#3"
        ]
    );
    // the second spoke is turned 90 degrees: it runs along +y
    let Op::Geometry(GeoOp::Solid {
        form: ddc_ops::Form::Extrude { frame, .. },
        ..
    }) = &r.env.groups[0].ops[7]
    else {
        panic!("extrude expected")
    };
    assert_eq!((frame.z.x, frame.z.y), (0.0, 1.0));
    assert_eq!(frame.origin.y, 5.0);
}

fn links_of(run: &ScriptRun, first_id: i64) -> Vec<Link> {
    run.parts
        .iter()
        .enumerate()
        .map(|(i, p)| Link {
            key: p.key.clone(),
            cat: Cat::Solid,
            id: first_id + i as i64,
        })
        .collect()
}

#[test]
fn rerun_touches_only_the_recipe_parts() {
    let old = Sandbox.run(BAYS, &opts(json!({"bays": 4}))).unwrap();
    let links = links_of(&old, 100);
    let actor = Actor::Script {
        name: "rerun".into(),
    };
    // same params: nothing to do
    let same = rerun_envelope(&old, &old, &links, actor.clone(), "rerun").unwrap();
    assert!(same.env.is_none());
    assert_eq!(same.kept, old.parts.len());

    // taller frame: every part changes in place, ids kept
    let tall = Sandbox
        .run(BAYS, &opts(json!({"bays": 4, "height": "4 m"})))
        .unwrap();
    let p = rerun_envelope(&old, &tall, &links, actor.clone(), "rerun").unwrap();
    assert_eq!(
        (p.reshaped.len(), p.created.len(), p.deleted.len()),
        (9, 0, 0)
    );

    // fewer bays: the last post and beam go, nothing else is named
    let fewer = Sandbox.run(BAYS, &opts(json!({"bays": 3}))).unwrap();
    let p = rerun_envelope(&old, &fewer, &links, actor.clone(), "rerun").unwrap();
    assert_eq!(p.kept, 7);
    assert_eq!(p.deleted, vec!["post#5".to_string(), "beam#4".to_string()]);
    let env = p.env.unwrap();
    assert!(env.check().is_empty());
    let mut named = vec![];
    for op in env.ops() {
        op.for_each_ref(&mut |r: &Ref| named.push(r.clone()));
    }
    let ours: Vec<Ref> = links.iter().map(|l| Ref::id(l.cat, l.id)).collect();
    assert!(
        named.iter().all(|r| ours.contains(r)),
        "only recipe elements: {named:?}"
    );
    assert!(matches!(
        env.ops().next(),
        Some(Op::Delete {
            target: Targets::Refs { .. }
        })
    ));

    // more bays: new keys are created with fresh slots
    let more = Sandbox.run(BAYS, &opts(json!({"bays": 5}))).unwrap();
    let p = rerun_envelope(&old, &more, &links, actor, "rerun").unwrap();
    assert_eq!(
        p.created.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(),
        ["post#6", "beam#5"]
    );
    assert_eq!(p.kept, 9);
}

#[test]
fn rerun_replaces_role_parts_whose_role_moves() {
    let src = ddc_script::recipes::recipe("wind_turbine").unwrap();
    let old = Sandbox.run(src, &opts(json!({}))).unwrap();
    let links = links_of(&old, 100);
    let actor = Actor::Script { name: "rerun".into() };
    let same = rerun_envelope(&old, &old, &links, actor.clone(), "rerun").unwrap();
    assert!(same.env.is_none());
    // a taller mast moves the hub: the tagged parts are deleted and made again (their component
    // rows go with the solids), never reshaped in place under a stale tag
    let tall = Sandbox.run(src, &opts(json!({"height": 110}))).unwrap();
    let p = rerun_envelope(&old, &tall, &links, actor, "rerun").unwrap();
    assert!(p.replaced.iter().any(|k| k.starts_with("blade1/")), "{:?}", p.replaced);
    assert!(p.replaced.iter().any(|k| k.starts_with("rotor/")), "{:?}", p.replaced);
    assert!(p.reshaped.iter().all(|k| k.starts_with("tower") || k == "nacelle"), "{:?}", p.reshaped);
}
